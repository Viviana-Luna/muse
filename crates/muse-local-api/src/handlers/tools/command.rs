async fn tool_command_run(
    policy: &FrozenExecutionPolicy,
    call: &ToolCall,
    tx: Option<&RuntimeSseSender>,
    allow_approved_external_path: bool,
    cancel_token: &RuntimeTurnCancel,
) -> ToolResult {
    let Some(command) = tool_arg_string(&call.arguments, "command") else {
        return tool_failed("command_run 缺少 command 参数。", "missing_command");
    };
    if let Some(reason) = crate::command_environment::command_containment_escape_reason(&command) {
        return tool_failed(reason, "command_containment_escape");
    }
    let timeout_ms = call
        .arguments
        .get("timeout_ms")
        .and_then(|value| value.as_u64())
        .unwrap_or(COMMAND_RUN_TIMEOUT_MS)
        .clamp(1_000, COMMAND_RUN_MAX_TIMEOUT_MS);
    let audit_output_enabled = command_audit_output_requested(call);
    let cwd = tool_arg_string(&call.arguments, "cwd").unwrap_or_else(|| ".".to_string());
    let cwd = match resolve_workspace_path(policy, &cwd, false, allow_approved_external_path) {
        Ok(path) => path,
        Err(err) => return tool_failed(err, "invalid_cwd"),
    };
    let mut prepared_audit = if audit_output_enabled {
        match prepare_command_audit_files() {
            Ok(audit) => Some(audit),
            Err(error) => {
                return tool_failed(
                    format!("启用命令审计输出失败：{error}"),
                    "audit_output_unavailable",
                );
            }
        }
    } else {
        None
    };
    #[cfg(windows)]
    let mut process = {
        let mut process = Command::new("powershell.exe");
        process
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
            .arg(&command);
        // 进程必须先挂起，避免在加入 Job Object 前抢先派生不受控的子进程。
        process
    };
    #[cfg(not(windows))]
    let mut process = {
        let mut process = Command::new("/bin/sh");
        process.arg("-lc").arg(&command);
        process
    };
    crate::command_environment::apply_command_environment(&mut process);
    process.current_dir(&cwd);
    process.stdout(Stdio::piped()).stderr(Stdio::piped());
    muse_core::process_supervision::configure_process_tree(&mut process);
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(err) => {
            if let Some(audit) = prepared_audit.take() {
                audit.cleanup().await;
            }
            return tool_failed(format!("命令执行失败：{err}"), "command_failed");
        }
    };
    let child_pid = child.id();
    let mut process_tree_guard = match muse_core::process_supervision::ProcessTreeGuard::attach(&child) {
        Ok(guard) => guard,
        Err(error) => {
            // 此时 PowerShell 仍处于 CREATE_SUSPENDED，失败必须先回收再返回，不能降级裸跑。
            let mut cleanup_notes = Vec::new();
            if let Err(cleanup_error) = child.start_kill() {
                cleanup_notes.push(format!("终止挂起进程失败：{cleanup_error}"));
            }
            if let Err(cleanup_error) = child.wait().await {
                cleanup_notes.push(format!("等待挂起进程退出失败：{cleanup_error}"));
            }
            if let Some(audit) = prepared_audit.take() {
                audit.cleanup().await;
            }
            let cleanup = if cleanup_notes.is_empty() {
                String::new()
            } else {
                format!("；{}", cleanup_notes.join("；"))
            };
            return tool_failed(
                format!("Windows 命令进程树安全隔离失败，已拒绝执行：{error}{cleanup}"),
                "command_containment_failed",
            );
        }
    };
    let (stdout_audit, stderr_audit) = prepared_audit
        .take()
        .map(PreparedCommandAuditFiles::into_streams)
        .map_or((None, None), |(stdout, stderr)| {
            (Some(stdout), Some(stderr))
        });
    let stdout_reader = spawn_command_output_reader(
        "stdout",
        child.stdout.take(),
        tx.cloned(),
        call.call_id.clone(),
        call.name.clone(),
        stdout_audit,
    );
    let stderr_reader = spawn_command_output_reader(
        "stderr",
        child.stderr.take(),
        tx.cloned(),
        call.call_id.clone(),
        call.name.clone(),
        stderr_audit,
    );
    let mut wait_handle = tokio::spawn(async move { child.wait().await });
    let timeout = tokio::time::sleep(Duration::from_millis(timeout_ms));
    tokio::pin!(timeout);
    let outcome = tokio::select! {
        output = &mut wait_handle => CommandRunOutcome::Finished(command_output_from_join(output)),
        _ = &mut timeout => {
            CommandRunOutcome::Timeout(terminate_command_process(
                child_pid,
                &process_tree_guard,
                &mut wait_handle,
            ).await)
        }
        _ = wait_for_turn_cancel(cancel_token) => {
            CommandRunOutcome::Cancelled(terminate_command_process(
                child_pid,
                &process_tree_guard,
                &mut wait_handle,
            ).await)
        }
    };
    // POSIX shell 正常退出不代表它派生的后台进程已经退出。根进程被 reap 后仍按
    // 进程组清理剩余成员，避免后台进程继续持有管道、写入文件或成为孤儿进程。
    #[cfg(unix)]
    {
        cleanup_finished_command_group(child_pid, &process_tree_guard).await;
        process_tree_guard.disarm();
    }
    // 根进程正常退出时也立即关闭 Job；KILL_ON_JOB_CLOSE 会清理仍持有管道的后台子孙进程。
    #[cfg(windows)]
    drop(process_tree_guard);
    let reader_cleanup_timeout_ms = if audit_output_enabled {
        COMMAND_AUDIT_READER_CLEANUP_TIMEOUT_MS
    } else {
        COMMAND_READER_CLEANUP_TIMEOUT_MS
    };
    let reader_deadline =
        tokio::time::Instant::now() + Duration::from_millis(reader_cleanup_timeout_ms);
    let stdout = join_command_output_reader(stdout_reader, reader_deadline, "stdout").await;
    let stderr = join_command_output_reader(stderr_reader, reader_deadline, "stderr").await;

    match outcome {
        CommandRunOutcome::Finished(Ok(status)) => {
            command_output_tool_result(&cwd, &command, timeout_ms, status, stdout, stderr)
        }
        CommandRunOutcome::Finished(Err(err)) => {
            tool_failed(format!("命令执行失败：{err}"), "command_failed")
        }
        CommandRunOutcome::Timeout(termination) => {
            command_terminated_tool_result(CommandTerminationToolResult {
                cwd: &cwd,
                command: &command,
                timeout_ms,
                reason: "timeout",
                message: format!("命令执行超过 {timeout_ms}ms，已终止运行中的子进程。"),
                termination,
                stdout,
                stderr,
            })
        }
        CommandRunOutcome::Cancelled(termination) => {
            command_terminated_tool_result(CommandTerminationToolResult {
                cwd: &cwd,
                command: &command,
                timeout_ms,
                reason: "turn_cancelled",
                message: "命令已因当前 turn 取消而终止。".to_string(),
                termination,
                stdout,
                stderr,
            })
        }
    }
}

enum CommandRunOutcome {
    Finished(std::io::Result<ExitStatus>),
    Timeout(CommandTerminationResult),
    Cancelled(CommandTerminationResult),
}

struct CommandTerminationResult {
    status: Option<ExitStatus>,
    error: Option<String>,
}

fn command_output_from_join(
    result: Result<std::io::Result<ExitStatus>, tokio::task::JoinError>,
) -> std::io::Result<ExitStatus> {
    match result {
        Ok(output) => output,
        Err(err) => Err(std::io::Error::other(format!("等待命令进程失败：{err}"))),
    }
}

fn spawn_command_output_reader<R>(
    stream: &'static str,
    reader: Option<R>,
    tx: Option<RuntimeSseSender>,
    call_id: String,
    name: String,
    audit: Option<CommandAuditStreamWriter>,
) -> tokio::task::JoinHandle<CommandStreamCapture>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let audit = audit.map(CommandAuditStreamSink::new);
    match reader {
        Some(reader) => tokio::spawn(read_command_output_stream(
            stream, reader, tx, call_id, name, audit,
        )),
        None => tokio::spawn(async move {
            let audit = match audit {
                Some(audit) => Some(audit.finish().await),
                None => None,
            };
            CommandStreamCapture {
                summary: String::new(),
                total_bytes: 0,
                omitted_bytes: 0,
                sse_omitted_bytes: 0,
                audit,
                reader_error: Some(format!("未取得命令 {stream} 管道。")),
            }
        }),
    }
}

async fn read_command_output_stream<R>(
    stream: &'static str,
    mut reader: R,
    tx: Option<RuntimeSseSender>,
    call_id: String,
    name: String,
    mut audit: Option<CommandAuditStreamSink>,
) -> CommandStreamCapture
where
    R: AsyncRead + Unpin,
{
    let mut summary = CommandOutputSummaryBuffer::new();
    let mut sse_delivered_bytes = 0u64;
    let mut sse_closed = false;
    let mut reader_error = None;
    let mut buffer = [0u8; 4096];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) => {
                reader_error = Some(format!("读取命令 {stream} 失败：{err}"));
                break;
            }
        };
        let bytes = &buffer[..read];
        summary.push(bytes);
        if let Some(audit) = audit.as_mut() {
            audit.push(bytes);
        }
        if !sse_closed
            && sse_delivered_bytes < COMMAND_OUTPUT_SSE_LIMIT_BYTES as u64
            && let Some(tx) = tx.as_ref()
        {
            let remaining = (COMMAND_OUTPUT_SSE_LIMIT_BYTES as u64)
                .saturating_sub(sse_delivered_bytes) as usize;
            let delta = &bytes[..bytes.len().min(remaining)];
            if !delta.is_empty() {
                match tx.try_send(Ok(axum::response::sse::Event::default().data(
                    runtime_command_output_delta_event(
                        &call_id,
                        &name,
                        stream,
                        &sanitize_command_output_bytes(delta),
                    )
                    .to_string(),
                ))) {
                    Ok(()) => {
                        sse_delivered_bytes =
                            sse_delivered_bytes.saturating_add(delta.len() as u64);
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        sse_closed = true;
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                }
            }
        }
    }
    let total_bytes = summary.total_bytes;
    let audit = match audit {
        Some(audit) => Some(audit.finish().await),
        None => None,
    };
    let summary = summary.finish(stream);
    CommandStreamCapture {
        summary: summary.text,
        total_bytes,
        omitted_bytes: summary.omitted_bytes,
        sse_omitted_bytes: total_bytes.saturating_sub(sse_delivered_bytes),
        audit,
        reader_error,
    }
}

async fn join_command_output_reader(
    mut handle: tokio::task::JoinHandle<CommandStreamCapture>,
    deadline: tokio::time::Instant,
    stream: &'static str,
) -> CommandStreamCapture {
    match tokio::time::timeout_at(deadline, &mut handle).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            CommandStreamCapture::reader_failure(format!("{stream} 输出读取任务异常结束：{err}"))
        }
        Err(_) => {
            handle.abort();
            let _ = handle.await;
            CommandStreamCapture::reader_failure(format!(
                "{stream} 输出读取超过当前命令剩余期限，已停止读取。"
            ))
        }
    }
}

async fn terminate_command_process(
    child_pid: Option<u32>,
    process_tree_guard: &muse_core::process_supervision::ProcessTreeGuard,
    wait_handle: &mut tokio::task::JoinHandle<std::io::Result<ExitStatus>>,
) -> CommandTerminationResult {
    let mut notes = Vec::<String>::new();
    let grace_deadline =
        tokio::time::Instant::now() + Duration::from_millis(COMMAND_TERMINATE_GRACE_MS);
    terminate_command_process_once(
        child_pid,
        process_tree_guard,
        CommandTerminationSignal::Terminate,
        &mut notes,
    );
    let root_result = tokio::time::timeout_at(grace_deadline, &mut *wait_handle)
        .await
        .ok();
    // 即使根 shell 已经先退出，也要给同组子孙完整 TERM 宽限期，再向整个组发送 KILL。
    tokio::time::sleep_until(grace_deadline).await;
    terminate_command_process_once(
        child_pid,
        process_tree_guard,
        CommandTerminationSignal::Kill,
        &mut notes,
    );

    if let Some(result) = root_result {
        return command_termination_result_from_join(result, notes);
    }

    match tokio::time::timeout(Duration::from_millis(2_000), &mut *wait_handle).await {
        Ok(result) => command_termination_result_from_join(result, notes),
        Err(_) => {
            wait_handle.abort();
            notes.push("等待命令进程退出超时，已中止等待任务。".to_string());
            CommandTerminationResult {
                status: None,
                error: Some(notes.join("；")),
            }
        }
    }
}

#[cfg(unix)]
async fn cleanup_finished_command_group(
    child_pid: Option<u32>,
    process_tree_guard: &muse_core::process_supervision::ProcessTreeGuard,
) {
    let Some(pid) = child_pid else {
        return;
    };
    let process_group = -(pid as libc::pid_t);
    // 信号 0 只探测进程组是否仍有成员；根 shell 已被 wait 回收时，普通命令会
    // 直接返回 ESRCH，不为每条命令增加宽限期延迟。
    if unsafe { libc::kill(process_group, 0) } != 0 {
        return;
    }
    let mut notes = Vec::new();
    terminate_command_process_once(
        child_pid,
        process_tree_guard,
        CommandTerminationSignal::Terminate,
        &mut notes,
    );
    tokio::time::sleep(Duration::from_millis(COMMAND_TERMINATE_GRACE_MS)).await;
    terminate_command_process_once(
        child_pid,
        process_tree_guard,
        CommandTerminationSignal::Kill,
        &mut notes,
    );
    if !notes.is_empty() {
        tracing::warn!(
            target: "muse::command",
            cleanup = %notes.join("；"),
            "清理正常退出命令遗留的进程组时出现异常"
        );
    }
}

#[derive(Debug, Clone, Copy)]
enum CommandTerminationSignal {
    Terminate,
    Kill,
}

fn terminate_command_process_once(
    child_pid: Option<u32>,
    process_tree_guard: &muse_core::process_supervision::ProcessTreeGuard,
    signal: CommandTerminationSignal,
    notes: &mut Vec<String>,
) {
    let _ = child_pid;
    if let Err(error) = process_tree_guard.terminate(matches!(signal, CommandTerminationSignal::Kill)) {
        notes.push(error);
    }
}

fn command_termination_result_from_join(
    result: Result<std::io::Result<ExitStatus>, tokio::task::JoinError>,
    mut notes: Vec<String>,
) -> CommandTerminationResult {
    match command_output_from_join(result) {
        Ok(status) => CommandTerminationResult {
            status: Some(status),
            error: if notes.is_empty() {
                None
            } else {
                Some(notes.join("；"))
            },
        },
        Err(err) => {
            notes.push(err.to_string());
            CommandTerminationResult {
                status: None,
                error: Some(notes.join("；")),
            }
        }
    }
}

fn command_output_tool_result(
    cwd: &StdPath,
    command: &str,
    timeout_ms: u64,
    status: ExitStatus,
    stdout: CommandStreamCapture,
    stderr: CommandStreamCapture,
) -> ToolResult {
    let stdout_error = command_stream_reader_error_text(&stdout);
    let stderr_error = command_stream_reader_error_text(&stderr);
    let content = format!(
        "命令退出状态：{}\n超时时间：{}ms\nstdout:\n{}{}\nstderr:\n{}{}",
        status,
        timeout_ms,
        stdout.summary.as_str(),
        stdout_error,
        stderr.summary.as_str(),
        stderr_error
    );
    ToolResult {
        status: ToolResultStatus::from_success(status.success()),
        content,
        structured: Some(serde_json::json!({
            "status": status.code(),
            "cwd": cwd,
            "command": command,
            "timeout_ms": timeout_ms,
            "streamed": true,
            "stdout": command_stream_capture_json(&stdout),
            "stderr": command_stream_capture_json(&stderr),
        })),
    }
}

fn command_stream_reader_error_text(capture: &CommandStreamCapture) -> String {
    capture
        .reader_error
        .as_ref()
        .map(|error| format!("\n[输出读取异常：{error}]\n"))
        .unwrap_or_default()
}

fn command_stream_capture_json(capture: &CommandStreamCapture) -> serde_json::Value {
    serde_json::json!({
        "total_bytes": capture.total_bytes,
        "summary_bytes": capture.summary.len(),
        "summary_preview": truncate_text_head_tail(
            &capture.summary,
            TOOL_RESULT_CONTENT_PREVIEW_CHARS,
        ),
        "omitted_bytes": capture.omitted_bytes,
        "sse_omitted_bytes": capture.sse_omitted_bytes,
        "reader_error": capture.reader_error.as_deref(),
        "audit_output": capture.audit.as_ref().map(CommandAuditReference::to_json),
    })
}

struct CommandTerminationToolResult<'a> {
    cwd: &'a StdPath,
    command: &'a str,
    timeout_ms: u64,
    reason: &'a str,
    message: String,
    termination: CommandTerminationResult,
    stdout: CommandStreamCapture,
    stderr: CommandStreamCapture,
}

fn command_terminated_tool_result(input: CommandTerminationToolResult<'_>) -> ToolResult {
    let CommandTerminationToolResult {
        cwd,
        command,
        timeout_ms,
        reason,
        message,
        termination,
        stdout,
        stderr,
    } = input;
    let stdout_error = command_stream_reader_error_text(&stdout);
    let stderr_error = command_stream_reader_error_text(&stderr);
    let detail = termination
        .error
        .as_ref()
        .map(|error| format!("\n终止细节：{error}"))
        .unwrap_or_default();
    ToolResult {
        status: ToolResultStatus::Failed,
        content: format!(
            "{}{}\n超时时间：{}ms\nstdout:\n{}{}\nstderr:\n{}{}",
            message,
            detail,
            timeout_ms,
            stdout.summary.as_str(),
            stdout_error,
            stderr.summary.as_str(),
            stderr_error
        ),
        structured: Some(serde_json::json!({
            "reason": reason,
            "terminated": true,
            "status": termination.status.and_then(|status| status.code()),
            "cwd": cwd,
            "command": command,
            "timeout_ms": timeout_ms,
            "termination_error": termination.error,
            "streamed": true,
            "stdout": command_stream_capture_json(&stdout),
            "stderr": command_stream_capture_json(&stderr),
        })),
    }
}

const WEB_RESPONSE_MAX_BYTES: usize = 1_000_000;
const WEB_FETCH_TIMEOUT_SECS: u64 = 20;
const WEB_FETCH_MAX_REDIRECTS: usize = 3;

#[derive(Clone)]
struct ValidatedPublicHttpsUrl {
    url: reqwest::Url,
    /// 域名解析结果固定到本次请求客户端，避免校验后再次解析造成 DNS rebinding。
    pinned_host: Option<(String, std::net::SocketAddr)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RestrictedHttpsProxyPolicy {
    /// 受限网络请求必须绕过系统和环境代理，确保连接使用本地校验的 DNS pin。
    Disabled,
}

const RESTRICTED_HTTPS_PROXY_POLICY: RestrictedHttpsProxyPolicy =
    RestrictedHttpsProxyPolicy::Disabled;

/// 只允许访问公开 HTTPS 地址，避免网页工具被用于访问本机、内网或云元数据服务。
async fn validate_public_https_url(value: &str) -> Result<ValidatedPublicHttpsUrl, String> {
    let url = reqwest::Url::parse(value).map_err(|err| format!("URL 格式无效：{err}"))?;
    if url.scheme() != "https" {
        return Err("网页访问只允许 HTTPS 地址。".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("网页地址不能包含用户名或密码。".to_string());
    }
    if url.fragment().is_some() {
        return Err("网页地址不能包含片段标识。".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "URL 缺少主机名。".to_string())?;
    let normalized_host = host.to_ascii_lowercase();
    if normalized_host == "localhost"
        || normalized_host.ends_with(".localhost")
        || normalized_host.ends_with(".local")
    {
        return Err("网页访问不允许本机或局域网主机名。".to_string());
    }
    let pinned_host = if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if is_non_public_ip(ip) {
            return Err("网页访问不允许本机、内网或保留 IP 地址。".to_string());
        }
        None
    } else {
        let port = url.port_or_known_default().unwrap_or(443);
        let addresses = tokio::net::lookup_host((host, port))
            .await
            .map_err(|err| format!("无法解析网页主机：{err}"))?
            .collect::<Vec<_>>();
        if addresses.is_empty()
            || addresses
                .iter()
                .any(|address| is_non_public_ip(address.ip()))
        {
            return Err("网页主机未解析到公开地址，已拒绝访问。".to_string());
        }
        Some((host.to_string(), addresses[0]))
    };
    Ok(ValidatedPublicHttpsUrl { url, pinned_host })
}

fn build_pinned_https_client(
    target: &ValidatedPublicHttpsUrl,
    timeout: Duration,
) -> Result<reqwest::Client, String> {
    let builder = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none());
    // 受限客户端必须直接连接已经校验并固定的地址。若继承 HTTP(S)_PROXY，
    // 代理会重新解析域名，既绕过 DNS pin，也可能接触下载凭据或搜索密钥。
    let mut builder = match RESTRICTED_HTTPS_PROXY_POLICY {
        RestrictedHttpsProxyPolicy::Disabled => builder.no_proxy(),
    };
    if let Some((host, address)) = &target.pinned_host {
        builder = builder.resolve(host, *address);
    }
    builder
        .build()
        .map_err(|err| format!("无法创建受限 HTTPS 客户端：{err}"))
}

/// 判断 IP 是否属于不应由模型驱动网络工具访问的地址范围。
fn is_non_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let octets = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_unspecified()
                || ip.is_multicast()
                || octets[0] == 0
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 169 && octets[1] == 254)
                || (octets[0] == 172 && (16..=31).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
                || (octets[0] == 192 && octets[1] == 88 && octets[2] == 99)
                || (octets[0] == 192 && octets[1] == 168)
                || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
                || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
                || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
                || octets[0] >= 240
        }
        std::net::IpAddr::V6(ip) => {
            if let Some(embedded) = ip.to_ipv4() {
                return is_non_public_ip(std::net::IpAddr::V4(embedded));
            }
            let segments = ip.segments();
            let is_global_unicast = (segments[0] & 0xe000) == 0x2000;
            !is_global_unicast
                // IETF 协议分配、ORCHID、Teredo 等 2001::/23 不作为网页目标。
                || (segments[0] == 0x2001 && segments[1] <= 0x01ff)
                // RFC 3849 文档地址。
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
                // 6to4 会把 IPv4 目标嵌入地址，拒绝以免绕过 IPv4 范围判断。
                || segments[0] == 0x2002
                // 已废弃的 6bone 前缀。
                || segments[0] == 0x3ffe
        }
    }
}

/// 读取受限大小的文本响应，防止分块响应绕过 Content-Length 检查。
async fn read_bounded_web_response(response: reqwest::Response) -> Result<String, String> {
    if response
        .content_length()
        .is_some_and(|length| length > WEB_RESPONSE_MAX_BYTES as u64)
    {
        return Err(format!(
            "网页响应超过 {} 字节上限。",
            WEB_RESPONSE_MAX_BYTES
        ));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| format!("读取网页响应失败：{err}"))?;
        if bytes.len().saturating_add(chunk.len()) > WEB_RESPONSE_MAX_BYTES {
            return Err(format!(
                "网页响应超过 {} 字节上限。",
                WEB_RESPONSE_MAX_BYTES
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
