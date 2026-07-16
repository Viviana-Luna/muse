async fn tool_file_read(
    policy: &FrozenExecutionPolicy,
    call: &ToolCall,
    allow_approved_external_path: bool,
) -> ToolResult {
    let Some(path) = tool_arg_string(&call.arguments, "path") else {
        return tool_failed("file_read 缺少 path 参数。", "missing_path");
    };
    let path = match resolve_workspace_path(policy, &path, false, allow_approved_external_path) {
        Ok(path) => path,
        Err(err) => return tool_failed(err, "invalid_path"),
    };
    match fs::read_to_string(&path).await {
        Ok(content) => ToolResult {
            status: ToolResultStatus::Success,
            content: content.clone(),
            structured: Some(serde_json::json!({
                "path": path,
                "chars": content.chars().count(),
            })),
        },
        Err(err) => tool_failed(format!("读取文件失败：{err}"), "read_failed"),
    }
}
async fn tool_file_list(
    policy: &FrozenExecutionPolicy,
    call: &ToolCall,
    allow_approved_external_path: bool,
) -> ToolResult {
    let path = tool_arg_string(&call.arguments, "path").unwrap_or_else(|| ".".to_string());
    let path = match resolve_workspace_path(policy, &path, false, allow_approved_external_path) {
        Ok(path) => path,
        Err(err) => return tool_failed(err, "invalid_path"),
    };
    let mut entries = Vec::new();
    let mut dir = match fs::read_dir(&path).await {
        Ok(dir) => dir,
        Err(err) => return tool_failed(format!("读取目录失败：{err}"), "read_dir_failed"),
    };
    while let Ok(Some(entry)) = dir.next_entry().await {
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        entries.push(serde_json::json!({
            "name": entry.file_name().to_string_lossy(),
            "path": entry.path(),
            "kind": if meta.is_dir() { "dir" } else { "file" },
            "size": meta.len(),
        }));
        if entries.len() >= 200 {
            break;
        }
    }
    ToolResult {
        status: ToolResultStatus::Success,
        content: format!(
            "目录 `{}` 中找到 {} 个条目。",
            path.display(),
            entries.len()
        ),
        structured: Some(serde_json::json!({ "path": path, "entries": entries })),
    }
}

async fn tool_file_search(policy: &FrozenExecutionPolicy, call: &ToolCall) -> ToolResult {
    let Some(query) = tool_arg_string(&call.arguments, "query") else {
        return tool_failed("file_search 缺少 query 参数。", "missing_query");
    };
    let raw_base = tool_arg_string(&call.arguments, "base").unwrap_or_else(|| ".".to_string());
    let base = match resolve_file_search_base(policy, &raw_base) {
        Ok(path) => path,
        Err(err) => return tool_failed(err, "invalid_path"),
    };
    if is_filesystem_root(&base) {
        return tool_failed(
            "file_search 不允许直接从磁盘根目录开始搜索，请提供更窄的 base。",
            "base_too_broad",
        );
    }

    let kind = tool_arg_string(&call.arguments, "kind").unwrap_or_else(|| "any".to_string());
    if !matches!(kind.as_str(), "any" | "file" | "directory") {
        return tool_failed(
            "file_search kind 只支持 any、file、directory。",
            "invalid_kind",
        );
    }
    let match_mode =
        tool_arg_string(&call.arguments, "match").unwrap_or_else(|| "name".to_string());
    if !matches!(match_mode.as_str(), "name" | "path" | "content") {
        return tool_failed(
            "file_search match 只支持 name、path、content。",
            "invalid_match",
        );
    }

    let limit = tool_arg_limit(&call.arguments, 20, 100);
    let max_depth = call
        .arguments
        .get("max_depth")
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .unwrap_or(4)
        .min(8);
    let include_hidden = call
        .arguments
        .get("include_hidden")
        .and_then(|value| value.as_bool())
        .unwrap_or_else(|| query.starts_with('.'));

    let metadata = match fs::metadata(&base).await {
        Ok(metadata) => metadata,
        Err(err) => {
            return tool_failed(
                format!("搜索起点不存在或无法访问：{}，错误：{err}", base.display()),
                "base_unavailable",
            );
        }
    };

    let query_lower = query.to_lowercase();
    let mut results = Vec::new();
    if metadata.is_file() {
        maybe_push_file_search_result(
            &mut results,
            FileSearchCandidate {
                path: &base,
                base: &base,
                metadata: &metadata,
                query_lower: &query_lower,
                kind: &kind,
                match_mode: &match_mode,
                include_hidden,
            },
        )
        .await;
    } else if metadata.is_dir() {
        let mut queue = VecDeque::new();
        queue.push_back((base.clone(), 0usize));
        while let Some((current, depth)) = queue.pop_front() {
            if results.len() >= limit {
                break;
            }
            let mut dir = match fs::read_dir(&current).await {
                Ok(dir) => dir,
                Err(_) => continue,
            };
            while let Ok(Some(entry)) = dir.next_entry().await {
                if results.len() >= limit {
                    break;
                }
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                let hidden = name.starts_with('.');
                if hidden && !include_hidden {
                    continue;
                }
                let Ok(file_type) = entry.file_type().await else {
                    continue;
                };
                // 不跟随目录内的符号链接，避免从已授权根目录逃逸到其他位置。
                if file_type.is_symlink() {
                    continue;
                }
                let Ok(metadata) = entry.metadata().await else {
                    continue;
                };
                maybe_push_file_search_result(
                    &mut results,
                    FileSearchCandidate {
                        path: &path,
                        base: &base,
                        metadata: &metadata,
                        query_lower: &query_lower,
                        kind: &kind,
                        match_mode: &match_mode,
                        include_hidden,
                    },
                )
                .await;
                if metadata.is_dir() && depth + 1 < max_depth {
                    queue.push_back((path, depth + 1));
                }
            }
        }
    } else {
        return tool_failed(
            "file_search 的 base 必须是文件或目录。",
            "invalid_base_kind",
        );
    }

    let content = if results.is_empty() {
        format!(
            "在 `{}` 下没有找到 `{}` 的候选路径。",
            base.display(),
            query
        )
    } else {
        let candidates = results
            .iter()
            .take(20)
            .filter_map(|item| item.get("path").and_then(|value| value.as_str()))
            .map(|path| format!("- `{path}`"))
            .collect::<Vec<_>>();
        let more = results.len().saturating_sub(candidates.len());
        let more_line = if more > 0 {
            format!("\n... 另有 {more} 个候选。")
        } else {
            String::new()
        };
        format!(
            "在 `{}` 下找到 {} 个 `{}` 的候选路径：\n{}{}",
            base.display(),
            results.len(),
            query,
            candidates.join("\n"),
            more_line
        )
    };

    ToolResult {
        status: ToolResultStatus::Success,
        content,
        structured: Some(serde_json::json!({
            "query": query,
            "base": base.display().to_string(),
            "kind": kind,
            "match": match_mode,
            "max_depth": max_depth,
            "include_hidden": include_hidden,
            "results": results,
        })),
    }
}

/// 解析 file_search 的搜索起点。该工具无论当前是否 full_access、是否经过审批，
/// 都只能使用冻结策略与当前撤销策略交集后的 canonical 根目录。
fn resolve_file_search_base(policy: &FrozenExecutionPolicy, raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("file_search 的 base 不能为空。".to_string());
    }
    if raw == "~" || raw.starts_with("~/") || raw.starts_with("~\\") {
        return Err("file_search 不允许使用 `~` 路径。".to_string());
    }
    let raw_path = StdPath::new(raw);
    if raw_path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("file_search 的 base 不能包含上级目录跳转。".to_string());
    }

    let workspace = workspace_root()?;
    let candidate = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        workspace.join(raw_path)
    };
    let canonical = candidate
        .canonicalize()
        .map_err(|err| format!("file_search 的 base 不存在或无法解析：{err}"))?;
    if is_filesystem_root(&canonical) {
        return Err("file_search 不允许从文件系统根目录开始搜索。".to_string());
    }

    let allowed = policy.allowed_roots.iter().any(|root| {
        let root = root.canonicalize().unwrap_or_else(|_| root.clone());
        canonical.starts_with(root)
    });
    if !allowed {
        return Err("file_search 的 base 超出当前冻结工作区，已拒绝。".to_string());
    }
    Ok(canonical)
}

fn is_filesystem_root(path: &StdPath) -> bool {
    path.parent().is_none()
}

struct FileSearchCandidate<'a> {
    path: &'a StdPath,
    base: &'a StdPath,
    metadata: &'a std::fs::Metadata,
    query_lower: &'a str,
    kind: &'a str,
    match_mode: &'a str,
    include_hidden: bool,
}

async fn maybe_push_file_search_result(
    results: &mut Vec<serde_json::Value>,
    candidate: FileSearchCandidate<'_>,
) {
    let FileSearchCandidate {
        path,
        base,
        metadata,
        query_lower,
        kind,
        match_mode,
        include_hidden,
    } = candidate;
    let is_dir = metadata.is_dir();
    let is_file = metadata.is_file();
    if (kind == "file" && !is_file) || (kind == "directory" && !is_dir) {
        return;
    }

    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    if name.starts_with('.') && !include_hidden {
        return;
    }
    let relative = path
        .strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace("\\", "/");
    let matched = match match_mode {
        "path" => relative.to_lowercase().contains(query_lower),
        "content" => is_file && file_content_contains(path, query_lower).await,
        _ => name.to_lowercase().contains(query_lower),
    };
    if !matched {
        return;
    }

    results.push(serde_json::json!({
        "path": path.display().to_string(),
        "relative_path": relative,
        "name": name,
        "kind": if is_dir { "directory" } else if is_file { "file" } else { "other" },
        "size": metadata.len(),
        "hidden": path.file_name().is_some_and(|value| value.to_string_lossy().starts_with('.')),
        "match": match_mode,
    }));
}

async fn file_content_contains(path: &StdPath, query_lower: &str) -> bool {
    const MAX_CONTENT_SEARCH_BYTES: u64 = 256 * 1024;
    let Ok(metadata) = fs::metadata(path).await else {
        return false;
    };
    if metadata.len() > MAX_CONTENT_SEARCH_BYTES {
        return false;
    }
    let Ok(bytes) = fs::read(path).await else {
        return false;
    };
    if bytes.contains(&0) {
        return false;
    }
    String::from_utf8(bytes)
        .map(|content| content.to_lowercase().contains(query_lower))
        .unwrap_or(false)
}

async fn tool_file_write(
    policy: &FrozenExecutionPolicy,
    call: &ToolCall,
    allow_approved_external_path: bool,
) -> ToolResult {
    let Some(path) = tool_arg_string(&call.arguments, "path") else {
        return tool_failed("file_write 缺少 path 参数。", "missing_path");
    };
    let Some(content) = call
        .arguments
        .get("content")
        .and_then(|value| value.as_str())
    else {
        return tool_failed("file_write 缺少 content 参数。", "missing_content");
    };
    let path = match resolve_workspace_path(policy, &path, true, allow_approved_external_path) {
        Ok(path) => path,
        Err(err) => return tool_failed(err, "invalid_path"),
    };
    let Some(parent) = path.parent() else {
        return tool_failed("目标文件缺少父目录。", "missing_parent");
    };
    if !parent.exists() {
        return tool_failed(
            format!("目标父目录不存在：{}", parent.display()),
            "parent_not_found",
        );
    }
    if !parent.is_dir() {
        return tool_failed(
            format!("目标父路径不是目录：{}", parent.display()),
            "parent_not_directory",
        );
    }
    let existed = path.exists();
    match fs::write(&path, content).await {
        Ok(_) => ToolResult {
            status: ToolResultStatus::Success,
            content: format!(
                "{}文件：{}",
                if existed { "已覆盖" } else { "已创建" },
                path.display()
            ),
            structured: Some(serde_json::json!({ "path": path, "existed": existed })),
        },
        Err(err) => tool_failed(format!("写入文件失败：{err}"), "write_failed"),
    }
}

async fn tool_file_edit(
    policy: &FrozenExecutionPolicy,
    call: &ToolCall,
    allow_approved_external_path: bool,
) -> ToolResult {
    let Some(path) = tool_arg_string(&call.arguments, "path") else {
        return tool_failed("file_edit 缺少 path 参数。", "missing_path");
    };
    let Some(old_string) = call
        .arguments
        .get("old_string")
        .and_then(|value| value.as_str())
    else {
        return tool_failed("file_edit 缺少 old_string 参数。", "missing_old_string");
    };
    let Some(new_string) = call
        .arguments
        .get("new_string")
        .and_then(|value| value.as_str())
    else {
        return tool_failed("file_edit 缺少 new_string 参数。", "missing_new_string");
    };
    if old_string == new_string {
        return tool_failed(
            "old_string 与 new_string 完全相同，没有可编辑内容。",
            "no_change",
        );
    }
    let path = match resolve_workspace_path(policy, &path, true, allow_approved_external_path) {
        Ok(path) => path,
        Err(err) => return tool_failed(err, "invalid_path"),
    };
    let content = match fs::read_to_string(&path).await {
        Ok(content) => content,
        Err(err) => return tool_failed(format!("读取待编辑文件失败：{err}"), "read_failed"),
    };
    let count = content.matches(old_string).count();
    if count == 0 {
        return tool_failed("没有找到 old_string，已拒绝编辑。", "old_string_not_found");
    }
    if count > 1 {
        return tool_failed(
            "old_string 在文件中不唯一，已拒绝编辑。",
            "old_string_not_unique",
        );
    }
    let updated = content.replacen(old_string, new_string, 1);
    match fs::write(&path, updated).await {
        Ok(_) => ToolResult {
            status: ToolResultStatus::Success,
            content: format!("已编辑文件：{}", path.display()),
            structured: Some(serde_json::json!({ "path": path })),
        },
        Err(err) => tool_failed(format!("写入编辑结果失败：{err}"), "write_failed"),
    }
}

fn command_audit_output_requested(call: &ToolCall) -> bool {
    call.arguments
        .get("audit_output")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

struct CommandOutputSummaryBuffer {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total_bytes: u64,
}

struct CommandOutputSummary {
    text: String,
    omitted_bytes: u64,
}

impl CommandOutputSummaryBuffer {
    fn new() -> Self {
        Self {
            head: Vec::with_capacity(COMMAND_OUTPUT_MEMORY_LIMIT_BYTES / 2),
            tail: VecDeque::with_capacity(COMMAND_OUTPUT_MEMORY_LIMIT_BYTES / 2),
            total_bytes: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(bytes.len() as u64);
        let head_capacity = COMMAND_OUTPUT_MEMORY_LIMIT_BYTES / 2;
        let head_take = bytes
            .len()
            .min(head_capacity.saturating_sub(self.head.len()));
        self.head.extend_from_slice(&bytes[..head_take]);

        let remaining = &bytes[head_take..];
        let tail_capacity = COMMAND_OUTPUT_MEMORY_LIMIT_BYTES.saturating_sub(head_capacity);
        if remaining.len() >= tail_capacity {
            self.tail.clear();
            self.tail.extend(
                remaining[remaining.len().saturating_sub(tail_capacity)..]
                    .iter()
                    .copied(),
            );
            return;
        }
        let overflow = self
            .tail
            .len()
            .saturating_add(remaining.len())
            .saturating_sub(tail_capacity);
        if overflow > 0 {
            self.tail.drain(..overflow);
        }
        self.tail.extend(remaining.iter().copied());
    }

    fn finish(mut self, stream: &'static str) -> CommandOutputSummary {
        if self.total_bytes <= COMMAND_OUTPUT_MEMORY_LIMIT_BYTES as u64 {
            let mut bytes = self.head;
            bytes.extend(self.tail);
            return CommandOutputSummary {
                text: sanitize_command_output_bytes(&bytes),
                omitted_bytes: 0,
            };
        }

        let mut omitted_bytes = self
            .total_bytes
            .saturating_sub(COMMAND_OUTPUT_MEMORY_LIMIT_BYTES as u64);
        let (marker, head_keep, tail_keep) = loop {
            let marker = command_output_omission_marker(stream, self.total_bytes, omitted_bytes);
            let body_limit = COMMAND_OUTPUT_MEMORY_LIMIT_BYTES.saturating_sub(marker.len());
            let head_keep = body_limit.div_ceil(2);
            let tail_keep = body_limit.saturating_sub(head_keep);
            let adjusted_omitted = self
                .total_bytes
                .saturating_sub(head_keep.saturating_add(tail_keep) as u64);
            if adjusted_omitted == omitted_bytes {
                break (marker, head_keep, tail_keep);
            }
            omitted_bytes = adjusted_omitted;
        };

        let tail = self.tail.make_contiguous();
        let tail_start = tail.len().saturating_sub(tail_keep);
        let mut text = String::with_capacity(COMMAND_OUTPUT_MEMORY_LIMIT_BYTES);
        text.push_str(&sanitize_command_output_bytes(
            &self.head[..head_keep.min(self.head.len())],
        ));
        text.push_str(&marker);
        text.push_str(&sanitize_command_output_bytes(&tail[tail_start..]));
        debug_assert!(text.len() <= COMMAND_OUTPUT_MEMORY_LIMIT_BYTES);
        CommandOutputSummary {
            text,
            omitted_bytes,
        }
    }
}

fn command_output_omission_marker(
    stream: &'static str,
    total_bytes: u64,
    omitted_bytes: u64,
) -> String {
    format!(
        "\n[{stream} 输出共 {total_bytes} 字节，中间 {omitted_bytes} 字节已省略；以下继续显示尾部。]\n"
    )
}

/// 将任意命令字节转换成有效 UTF-8，同时保持输出字节数不大于输入字节数。
///
/// `String::from_utf8_lossy` 会把一个非法字节扩成三字节替换符，无法满足严格的
/// 1 MiB 上限。这里逐段保留有效 UTF-8，并把每个非法字节替换成单字节 `?`。
fn sanitize_command_output_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len());
    let mut offset = 0usize;
    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(valid) => {
                output.push_str(valid);
                break;
            }
            Err(error) => {
                let valid_end = offset.saturating_add(error.valid_up_to());
                if valid_end > offset {
                    // SAFETY: `valid_up_to` 保证此前字节是有效 UTF-8。
                    output.push_str(unsafe {
                        std::str::from_utf8_unchecked(&bytes[offset..valid_end])
                    });
                }
                let invalid_bytes = error
                    .error_len()
                    .unwrap_or_else(|| bytes.len().saturating_sub(valid_end));
                output.extend(std::iter::repeat_n('?', invalid_bytes));
                offset = valid_end.saturating_add(invalid_bytes);
            }
        }
    }
    output
}

#[derive(Clone)]
struct CommandAuditIdentity {
    audit_id: String,
    stream: &'static str,
    path: PathBuf,
}

struct CommandAuditStreamWriter {
    identity: CommandAuditIdentity,
    file: tokio::fs::File,
    written_bytes: u64,
    write_error: Option<String>,
}

struct CommandAuditStreamSink {
    identity: CommandAuditIdentity,
    sender: Option<mpsc::Sender<Vec<u8>>>,
    writer_task: tokio::task::JoinHandle<CommandAuditReference>,
    total_bytes: Arc<AtomicU64>,
    accepting_chunks: bool,
}

struct PreparedCommandAuditFiles {
    stdout: CommandAuditStreamWriter,
    stderr: CommandAuditStreamWriter,
}

#[derive(Clone)]
struct CommandAuditReference {
    audit_id: String,
    stream: &'static str,
    resource_uri: String,
    captured_bytes: u64,
    omitted_bytes: u64,
    write_error: Option<String>,
}

#[derive(Debug)]
struct CommandAuditResourceContent {
    content: String,
    captured_bytes: u64,
    truncated: bool,
}

async fn read_command_audit_resource(
    resource_uri: String,
) -> Result<CommandAuditResourceContent, String> {
    tokio::task::spawn_blocking(move || {
        read_command_audit_resource_from_base(
            &muse_core::config::Config::config_dir(),
            &resource_uri,
        )
    })
    .await
    .map_err(|error| format!("命令审计读取任务异常结束：{error}"))?
}

fn read_command_audit_resource_from_base(
    base_dir: &StdPath,
    resource_uri: &str,
) -> Result<CommandAuditResourceContent, String> {
    let suffix = resource_uri
        .strip_prefix(COMMAND_AUDIT_RESOURCE_PREFIX)
        .ok_or_else(|| "命令审计资源 URI 无效。".to_string())?;
    let mut parts = suffix.split('/');
    let audit_id = parts.next().unwrap_or_default();
    let stream = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || !crate::tool_result_archive::is_safe_result_id(audit_id)
        || !matches!(stream, "stdout" | "stderr")
    {
        return Err("命令审计资源 URI 含非法标识或流名称。".to_string());
    }
    let audit_dir = existing_command_audit_directory(base_dir)?;
    let path = audit_dir.join(format!("{audit_id}.{stream}.log"));
    let link_metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "命令审计资源不存在。".to_string()
        } else {
            format!("检查命令审计资源失败：{error}")
        }
    })?;
    if link_metadata.file_type().is_symlink() || !link_metadata.is_file() {
        return Err("命令审计资源不是可信普通文件。".to_string());
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(&path)
        .map_err(|error| format!("安全打开命令审计资源失败：{error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("读取命令审计资源状态失败：{error}"))?;
    if !metadata.is_file() {
        return Err("命令审计资源不是普通文件。".to_string());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err("命令审计资源不能是 Windows reparse point。".to_string());
        }
    }

    let captured_bytes = metadata.len();
    let mut reader = std::io::Read::take(file, COMMAND_AUDIT_OUTPUT_LIMIT_BYTES + 1);
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut bytes)
        .map_err(|error| format!("读取命令审计资源失败：{error}"))?;
    let truncated = captured_bytes > COMMAND_AUDIT_OUTPUT_LIMIT_BYTES
        || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > COMMAND_AUDIT_OUTPUT_LIMIT_BYTES;
    bytes.truncate(COMMAND_AUDIT_OUTPUT_LIMIT_BYTES as usize);
    Ok(CommandAuditResourceContent {
        content: String::from_utf8_lossy(&bytes).into_owned(),
        captured_bytes: captured_bytes.min(COMMAND_AUDIT_OUTPUT_LIMIT_BYTES),
        truncated,
    })
}

impl CommandAuditReference {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "audit_id": self.audit_id.as_str(),
            "stream": self.stream,
            "resource_uri": self.resource_uri.as_str(),
            "captured_bytes": self.captured_bytes,
            "omitted_bytes": self.omitted_bytes,
            "truncated": self.omitted_bytes > 0,
            "write_error": self.write_error.as_deref(),
        })
    }
}

impl CommandAuditStreamWriter {
    async fn write_chunk(&mut self, bytes: &[u8]) {
        if self.write_error.is_some() {
            return;
        }
        let remaining = COMMAND_AUDIT_OUTPUT_LIMIT_BYTES.saturating_sub(self.written_bytes);
        let write_len = bytes.len().min(remaining as usize);
        if write_len == 0 {
            return;
        }
        if let Err(error) = self.file.write_all(&bytes[..write_len]).await {
            self.write_error = Some(format!("写入命令审计输出失败：{error}"));
            return;
        }
        self.written_bytes = self.written_bytes.saturating_add(write_len as u64);
    }

    async fn finish(mut self, total_bytes: u64) -> CommandAuditReference {
        if self.write_error.is_none()
            && let Err(error) = self.file.flush().await
        {
            self.write_error = Some(format!("刷新命令审计输出失败：{error}"));
        }
        if self.write_error.is_none()
            && let Err(error) = self.file.sync_all().await
        {
            self.write_error = Some(format!("同步命令审计输出失败：{error}"));
        }
        let captured_bytes = match self.file.metadata().await {
            Ok(metadata) if metadata.is_file() => {
                metadata.len().min(COMMAND_AUDIT_OUTPUT_LIMIT_BYTES)
            }
            Ok(_) => {
                self.write_error = Some("命令审计输出不再是普通文件。".to_string());
                0
            }
            Err(error) => {
                self.write_error = Some(format!("读取命令审计输出状态失败：{error}"));
                0
            }
        };
        CommandAuditReference {
            audit_id: self.identity.audit_id.clone(),
            stream: self.identity.stream,
            resource_uri: format!(
                "muse://command-audit/{}/{}",
                self.identity.audit_id, self.identity.stream
            ),
            captured_bytes,
            omitted_bytes: total_bytes.saturating_sub(captured_bytes),
            write_error: self.write_error,
        }
    }
}

impl CommandAuditStreamSink {
    fn new(writer: CommandAuditStreamWriter) -> Self {
        let identity = writer.identity.clone();
        let total_bytes = Arc::new(AtomicU64::new(0));
        let total_for_writer = total_bytes.clone();
        let (sender, mut receiver) = mpsc::channel::<Vec<u8>>(COMMAND_AUDIT_CHANNEL_CAPACITY);
        let writer_task = tokio::spawn(async move {
            let mut writer = writer;
            while let Some(bytes) = receiver.recv().await {
                writer.write_chunk(&bytes).await;
            }
            writer
                .finish(total_for_writer.load(Ordering::Relaxed))
                .await
        });
        Self {
            identity,
            sender: Some(sender),
            writer_task,
            total_bytes,
            accepting_chunks: true,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.total_bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        if !self.accepting_chunks {
            return;
        }
        let Some(sender) = self.sender.as_ref() else {
            self.accepting_chunks = false;
            return;
        };
        match sender.try_reserve() {
            Ok(permit) => permit.send(bytes.to_vec()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(())) => {
                // 审计磁盘慢于命令输出时保留已排队的前缀，之后只累计省略字节；
                // 绝不能为了审计文件反压 stdout/stderr 管道。
                self.accepting_chunks = false;
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(())) => {
                self.accepting_chunks = false;
            }
        }
    }

    async fn finish(mut self) -> CommandAuditReference {
        self.sender.take();
        match self.writer_task.await {
            Ok(reference) => reference,
            Err(error) => command_audit_fallback_reference(
                &self.identity,
                self.total_bytes.load(Ordering::Relaxed),
                format!("命令审计写入任务异常结束：{error}"),
            ),
        }
    }
}

fn command_audit_fallback_reference(
    identity: &CommandAuditIdentity,
    total_bytes: u64,
    error: String,
) -> CommandAuditReference {
    let captured_bytes = std::fs::metadata(&identity.path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len().min(COMMAND_AUDIT_OUTPUT_LIMIT_BYTES))
        .unwrap_or(0);
    CommandAuditReference {
        audit_id: identity.audit_id.clone(),
        stream: identity.stream,
        resource_uri: format!(
            "muse://command-audit/{}/{}",
            identity.audit_id, identity.stream
        ),
        captured_bytes,
        omitted_bytes: total_bytes.saturating_sub(captured_bytes),
        write_error: Some(error),
    }
}

impl PreparedCommandAuditFiles {
    fn into_streams(self) -> (CommandAuditStreamWriter, CommandAuditStreamWriter) {
        (self.stdout, self.stderr)
    }

    async fn cleanup(self) {
        let stdout_path = self.stdout.identity.path.clone();
        let stderr_path = self.stderr.identity.path.clone();
        drop(self);
        let _ = tokio::fs::remove_file(stdout_path).await;
        let _ = tokio::fs::remove_file(stderr_path).await;
    }
}

fn prepare_command_audit_files() -> Result<PreparedCommandAuditFiles, String> {
    prepare_command_audit_files_in(&muse_core::config::Config::config_dir())
}

fn prepare_command_audit_files_in(base_dir: &StdPath) -> Result<PreparedCommandAuditFiles, String> {
    let audit_dir = ensure_command_audit_directory(base_dir)?;
    let audit_id = next_runtime_id("command-audit");
    if !crate::tool_result_archive::is_safe_result_id(&audit_id) {
        return Err("生成的命令审计标识不安全。".to_string());
    }
    let stdout = create_command_audit_stream(&audit_dir, &audit_id, "stdout")?;
    let stderr = match create_command_audit_stream(&audit_dir, &audit_id, "stderr") {
        Ok(stderr) => stderr,
        Err(error) => {
            let stdout_path = stdout.identity.path.clone();
            drop(stdout);
            let _ = std::fs::remove_file(stdout_path);
            return Err(error);
        }
    };
    if let Err(error) = sync_command_audit_directory(&audit_dir) {
        let stdout_path = stdout.identity.path.clone();
        let stderr_path = stderr.identity.path.clone();
        drop(stdout);
        drop(stderr);
        let _ = std::fs::remove_file(stdout_path);
        let _ = std::fs::remove_file(stderr_path);
        return Err(error);
    }
    Ok(PreparedCommandAuditFiles { stdout, stderr })
}

fn ensure_command_audit_directory(base_dir: &StdPath) -> Result<PathBuf, String> {
    std::fs::create_dir_all(base_dir)
        .map_err(|error| format!("创建 Muse 数据目录失败：{error}"))?;
    let canonical_base = std::fs::canonicalize(base_dir)
        .map_err(|error| format!("规范化 Muse 数据目录失败：{error}"))?;
    let mut current = canonical_base.clone();
    for component in ["harness", "command-audits"] {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    || metadata_is_windows_reparse_point(&metadata)
                    || !metadata.is_dir() =>
            {
                return Err("命令审计目录不能是符号链接或普通文件。".to_string());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current)
                    .map_err(|error| format!("创建命令审计目录失败：{error}"))?;
            }
            Err(error) => return Err(format!("检查命令审计目录失败：{error}")),
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&current, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("收紧命令审计目录权限失败：{error}"))?;
        }
    }
    let canonical_audit_dir = std::fs::canonicalize(&current)
        .map_err(|error| format!("规范化命令审计目录失败：{error}"))?;
    let expected = canonical_base.join("harness").join("command-audits");
    if canonical_audit_dir != expected {
        return Err("命令审计目录逃逸出 Muse 受控数据目录。".to_string());
    }
    Ok(canonical_audit_dir)
}

/// 只验证已经存在的审计目录，纯读取路径绝不能创建目录或文件。
fn existing_command_audit_directory(base_dir: &StdPath) -> Result<PathBuf, String> {
    let base_metadata = std::fs::symlink_metadata(base_dir).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "命令审计资源不存在。".to_string()
        } else {
            format!("检查 Muse 数据目录失败：{error}")
        }
    })?;
    if base_metadata.file_type().is_symlink()
        || metadata_is_windows_reparse_point(&base_metadata)
        || !base_metadata.is_dir()
    {
        return Err("Muse 数据目录不是可信普通目录。".to_string());
    }
    let canonical_base = std::fs::canonicalize(base_dir)
        .map_err(|error| format!("规范化 Muse 数据目录失败：{error}"))?;
    let mut current = canonical_base.clone();
    for component in ["harness", "command-audits"] {
        current.push(component);
        let metadata = std::fs::symlink_metadata(&current).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "命令审计资源不存在。".to_string()
            } else {
                format!("检查命令审计目录失败：{error}")
            }
        })?;
        if metadata.file_type().is_symlink()
            || metadata_is_windows_reparse_point(&metadata)
            || !metadata.is_dir()
        {
            return Err("命令审计目录不能是符号链接、reparse point 或普通文件。".to_string());
        }
    }
    let canonical_audit_dir = std::fs::canonicalize(&current)
        .map_err(|error| format!("规范化命令审计目录失败：{error}"))?;
    if canonical_audit_dir != canonical_base.join("harness").join("command-audits") {
        return Err("命令审计目录逃逸出 Muse 受控数据目录。".to_string());
    }
    Ok(canonical_audit_dir)
}

#[cfg(windows)]
fn metadata_is_windows_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_windows_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

fn create_command_audit_stream(
    audit_dir: &StdPath,
    audit_id: &str,
    stream: &'static str,
) -> Result<CommandAuditStreamWriter, String> {
    if !matches!(stream, "stdout" | "stderr")
        || !crate::tool_result_archive::is_safe_result_id(audit_id)
    {
        return Err("命令审计文件标识无效。".to_string());
    }
    let path = audit_dir.join(format!("{audit_id}.{stream}.log"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(&path)
        .map_err(|error| format!("安全创建命令审计文件失败：{error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("读取命令审计文件状态失败：{error}"))?;
    if !metadata.is_file() {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err("新建的命令审计目标不是普通文件。".to_string());
    }
    Ok(CommandAuditStreamWriter {
        identity: CommandAuditIdentity {
            audit_id: audit_id.to_string(),
            stream,
            path,
        },
        file: tokio::fs::File::from_std(file),
        written_bytes: 0,
        write_error: None,
    })
}

#[cfg(unix)]
fn sync_command_audit_directory(path: &StdPath) -> Result<(), String> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("同步命令审计目录失败：{error}"))
}

#[cfg(not(unix))]
fn sync_command_audit_directory(_path: &StdPath) -> Result<(), String> {
    Ok(())
}

struct CommandStreamCapture {
    summary: String,
    total_bytes: u64,
    omitted_bytes: u64,
    sse_omitted_bytes: u64,
    audit: Option<CommandAuditReference>,
    reader_error: Option<String>,
}

impl CommandStreamCapture {
    fn reader_failure(message: String) -> Self {
        Self {
            summary: String::new(),
            total_bytes: 0,
            omitted_bytes: 0,
            sse_omitted_bytes: 0,
            audit: None,
            reader_error: Some(message),
        }
    }
}

/// Unix 命令进程组的同步兜底守卫。
///
/// `TurnSnapshot::run_with_deadline` 到达整回合硬期限时会直接丢弃工具 Future。
/// `Child` 此时已经移动到独立 wait task，单靠 `kill_on_drop` 无法终止它；守卫必须
/// 在 Future 的 Drop 路径同步强杀 PGID。正常退出路径完成进程组清理后再解除守卫。
#[cfg(unix)]
struct UnixCommandProcessGroupDropGuard {
    child_pid: Option<u32>,
    armed: bool,
}

#[cfg(unix)]
impl UnixCommandProcessGroupDropGuard {
    fn new(child_pid: Option<u32>) -> Self {
        Self {
            child_pid,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(unix)]
impl Drop for UnixCommandProcessGroupDropGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut notes = Vec::new();
        terminate_command_process_once(self.child_pid, CommandTerminationSignal::Kill, &mut notes);
        if !notes.is_empty() {
            tracing::error!(
                child_pid = self.child_pid,
                cleanup = %notes.join("；"),
                "整回合硬期限丢弃命令 Future 时，进程组兜底清理出现异常"
            );
        }
    }
}
