//! 网页共享状态模块，集中持有模型、会话、资源、角色、音色和运行底座状态。

use muse_core::app::preferences::MuseConfigStore;
use muse_core::app::secret::PlatformSecretStore;
use muse_core::config::Config;
use muse_core::domain::conversation::Conversation;
use muse_core::domain::mcp::migration::migrate_mcp_profiles;
use muse_core::domain::persona::Persona;
use muse_core::domain::persona::character::store::{PersonaStore, PersonaStoreError};
use muse_core::domain::persona::visual::store::VisualPackStore;
use muse_core::domain::runtime::RuntimeModeState;
use muse_core::domain::skill::migrate_legacy_skill_enabled;
use muse_core::domain::tool::{ToolDef, ToolRegistry, builtin};
use muse_core::model::config::{LlmConfig, ModelConfigStore, SpeechRecognitionConfig, TtsConfig};
use muse_core::model::migration::migrate_model_profiles;
use muse_core::model::provider::{ChatModelError, ChatModelProvider, factory};
use muse_core::speech::{SpeechRecognitionProvider, TtsProvider};
use muse_runtime::{FrozenExecutionPolicy, service::RuntimeService};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex, broadcast};

static RUNTIME_SESSION_ID_COUNTER: AtomicU64 = AtomicU64::new(1);
const CHAT_REQUEST_REGISTRY_CAPACITY: usize = 4096;
const CHAT_REQUEST_REGISTRY_TTL_SECS: u64 = 7 * 24 * 60 * 60;
const NO_ACTIVE_PERSONA_SYSTEM_PROMPT: &str =
    "你是 Muse 的中性运行时助手。当前没有激活角色，不得假定、虚构或扮演任何角色。";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ChatRequestRecord {
    client_request_id: String,
    accepted_at: u64,
    terminal_at: Option<u64>,
}

/// 已受理聊天请求的有序、持久幂等登记表。
///
/// 每个 ID 使用摘要文件名，文件第一行记录受理事实，终态以追加行持久化。只有超过
/// TTL 的记录才按受理顺序淘汰；容量耗尽时拒绝新请求，绝不任意删除仍受保护的 ID。
pub struct ChatRequestRegistry {
    records: HashMap<String, ChatRequestRecord>,
    order: VecDeque<String>,
    storage_dir: Option<PathBuf>,
    capacity: usize,
    ttl_secs: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChatRequestRegistryError {
    Duplicate,
    CapacityExceeded,
    Persistence(String),
}

impl std::fmt::Display for ChatRequestRegistryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Duplicate => formatter.write_str("该 client_request_id 已受理"),
            Self::CapacityExceeded => formatter.write_str("聊天请求幂等登记表已满"),
            Self::Persistence(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ChatRequestRegistryError {}

impl ChatRequestRegistry {
    pub fn load_from_dir(config_dir: PathBuf) -> Result<Self, ChatRequestRegistryError> {
        let runtime_dir = config_dir.join("runtime");
        let storage_dir = runtime_dir.join("chat-requests");
        std::fs::create_dir_all(&storage_dir).map_err(|error| {
            ChatRequestRegistryError::Persistence(format!("创建聊天请求幂等目录失败：{error}"))
        })?;
        // 新建目录本身也属于幂等事实的一部分。Unix 上同步每一级目录，确保首次
        // 受理后即使断电重启，摘要文件所在目录仍然可见。
        for directory in [&config_dir, &runtime_dir, &storage_dir] {
            sync_chat_request_directory(directory)?;
        }
        let mut registry = Self {
            records: HashMap::new(),
            order: VecDeque::new(),
            storage_dir: Some(storage_dir.clone()),
            capacity: CHAT_REQUEST_REGISTRY_CAPACITY,
            ttl_secs: CHAT_REQUEST_REGISTRY_TTL_SECS,
        };
        let entries = std::fs::read_dir(&storage_dir).map_err(|error| {
            ChatRequestRegistryError::Persistence(format!("读取聊天请求幂等目录失败：{error}"))
        })?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut record = None::<ChatRequestRecord>;
            for line in content.lines().filter(|line| !line.trim().is_empty()) {
                let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                match event.get("kind").and_then(|value| value.as_str()) {
                    Some("accepted") => {
                        let Some(client_request_id) = event
                            .get("client_request_id")
                            .and_then(|value| value.as_str())
                        else {
                            continue;
                        };
                        let Some(accepted_at) =
                            event.get("accepted_at").and_then(|value| value.as_u64())
                        else {
                            continue;
                        };
                        record = Some(ChatRequestRecord {
                            client_request_id: client_request_id.to_string(),
                            accepted_at,
                            terminal_at: None,
                        });
                    }
                    Some("terminal") => {
                        if let Some(record) = record.as_mut() {
                            record.terminal_at =
                                event.get("terminal_at").and_then(|value| value.as_u64());
                        }
                    }
                    _ => {}
                }
            }
            if let Some(record) = record {
                registry
                    .records
                    .insert(record.client_request_id.clone(), record);
            }
        }
        let mut ordered = registry.records.values().cloned().collect::<Vec<_>>();
        ordered.sort_by_key(|record| record.accepted_at);
        registry.order = ordered
            .into_iter()
            .map(|record| record.client_request_id)
            .collect();
        registry.prune_expired(unix_timestamp_secs());
        Ok(registry)
    }

    pub fn in_memory() -> Self {
        Self {
            records: HashMap::new(),
            order: VecDeque::new(),
            storage_dir: None,
            capacity: CHAT_REQUEST_REGISTRY_CAPACITY,
            ttl_secs: CHAT_REQUEST_REGISTRY_TTL_SECS,
        }
    }

    #[cfg(test)]
    fn with_limits(storage_dir: Option<PathBuf>, capacity: usize, ttl_secs: u64) -> Self {
        Self {
            records: HashMap::new(),
            order: VecDeque::new(),
            storage_dir,
            capacity,
            ttl_secs,
        }
    }

    pub fn accept(&mut self, client_request_id: &str) -> Result<(), ChatRequestRegistryError> {
        let now = unix_timestamp_secs();
        self.prune_expired(now);
        if self.records.contains_key(client_request_id) {
            return Err(ChatRequestRegistryError::Duplicate);
        }
        if self.records.len() >= self.capacity {
            return Err(ChatRequestRegistryError::CapacityExceeded);
        }
        self.persist_event(
            client_request_id,
            &serde_json::json!({
                "kind": "accepted",
                "client_request_id": client_request_id,
                "accepted_at": now,
            }),
            true,
        )?;
        self.records.insert(
            client_request_id.to_string(),
            ChatRequestRecord {
                client_request_id: client_request_id.to_string(),
                accepted_at: now,
                terminal_at: None,
            },
        );
        self.order.push_back(client_request_id.to_string());
        Ok(())
    }

    /// 在占用 turn 前执行无副作用重复检查；真正登记仍由 `accept` 完成。
    pub fn contains(&mut self, client_request_id: &str) -> bool {
        self.prune_expired(unix_timestamp_secs());
        self.records.contains_key(client_request_id)
    }

    pub fn mark_terminal(
        &mut self,
        client_request_id: &str,
    ) -> Result<(), ChatRequestRegistryError> {
        let now = unix_timestamp_secs();
        let Some(record) = self.records.get(client_request_id) else {
            return Ok(());
        };
        if record.terminal_at.is_some() {
            return Ok(());
        }
        self.persist_event(
            client_request_id,
            &serde_json::json!({
                "kind": "terminal",
                "client_request_id": client_request_id,
                "terminal_at": now,
            }),
            false,
        )?;
        if let Some(record) = self.records.get_mut(client_request_id) {
            record.terminal_at = Some(now);
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn prune_expired(&mut self, now: u64) {
        let expired = self
            .order
            .iter()
            .filter(|id| {
                self.records.get(*id).is_none_or(|record| {
                    now.saturating_sub(record.terminal_at.unwrap_or(record.accepted_at))
                        >= self.ttl_secs
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        for id in expired {
            self.records.remove(&id);
            self.order.retain(|queued| queued != &id);
            if let Some(path) = self.event_path(&id)
                && std::fs::remove_file(&path).is_ok()
                && let Some(parent) = path.parent()
            {
                sync_chat_request_directory_best_effort(parent);
            }
        }
    }

    fn persist_event(
        &self,
        client_request_id: &str,
        event: &serde_json::Value,
        create_new: bool,
    ) -> Result<(), ChatRequestRegistryError> {
        let Some(path) = self.event_path(client_request_id) else {
            return Ok(());
        };
        let mut options = OpenOptions::new();
        options.write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_WRITE_THROUGH;

            options.custom_flags(FILE_FLAG_WRITE_THROUGH);
        }
        if create_new {
            options.create_new(true);
        } else {
            options.create(false).append(true);
        }
        let mut file = options.open(&path).map_err(|error| {
            if create_new && error.kind() == std::io::ErrorKind::AlreadyExists {
                ChatRequestRegistryError::Duplicate
            } else {
                ChatRequestRegistryError::Persistence(format!(
                    "持久化聊天请求幂等状态失败：{error}"
                ))
            }
        })?;
        let mut line = serde_json::to_vec(event).map_err(|error| {
            ChatRequestRegistryError::Persistence(format!("序列化聊天请求幂等状态失败：{error}"))
        })?;
        line.push(b'\n');
        file.write_all(&line)
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                ChatRequestRegistryError::Persistence(format!("同步聊天请求幂等状态失败：{error}"))
            })?;
        if create_new && let Some(parent) = path.parent() {
            sync_chat_request_directory(parent)?;
        }
        Ok(())
    }

    fn event_path(&self, client_request_id: &str) -> Option<PathBuf> {
        let storage_dir = self.storage_dir.as_ref()?;
        let digest = Sha256::digest(client_request_id.as_bytes());
        Some(storage_dir.join(format!("{digest:x}.jsonl")))
    }
}

#[cfg(unix)]
fn sync_chat_request_directory(path: &std::path::Path) -> Result<(), ChatRequestRegistryError> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            ChatRequestRegistryError::Persistence(format!("同步聊天请求幂等目录失败：{error}"))
        })
}

#[cfg(not(unix))]
fn sync_chat_request_directory(_path: &std::path::Path) -> Result<(), ChatRequestRegistryError> {
    Ok(())
}

fn sync_chat_request_directory_best_effort(path: &std::path::Path) {
    if let Err(error) = sync_chat_request_directory(path) {
        tracing::warn!(path = %path.display(), %error, "清理过期聊天请求后同步目录失败");
    }
}

fn unix_timestamp_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

/// 生成新的运行时会话标识。
///
/// `default` 只作为旧 transcript 缺少会话 ID 时的兼容值，不再作为新对话的当前会话。
pub(crate) fn next_runtime_session_id() -> String {
    let seq = RUNTIME_SESSION_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("session-{}-{seq}", chrono::Utc::now().timestamp_millis())
}

/// 网页适配层共享状态，所有处理器都通过它访问运行时对象。
pub struct AppState {
    pub config: Config,
    /// 系统凭据库仅承载联网搜索凭据和启动期旧配置迁移；稳态模型与 MCP API Key 均归入 config.toml。
    pub secrets: PlatformSecretStore,
    /// 聊天主模型 provider；未配置真实模型时为空。
    /// 读端 `lock().await.clone()` 拿到共享指针后立即放锁，不阻塞并发。
    pub provider: Mutex<Option<Arc<dyn ChatModelProvider>>>,
    /// 语音合成提供器，未启用时为空。
    pub tts_provider: Mutex<Option<Arc<dyn TtsProvider>>>,
    /// 远程语音识别器，未启用时为空。
    pub speech_recognition_provider: Mutex<Option<Arc<dyn SpeechRecognitionProvider>>>,
    /// 模型配置运行时存储，承载聊天、语音合成、语音识别和音频理解配置，可热更新。
    pub model_config: Arc<Mutex<MuseConfigStore>>,
    /// 用户级声明式配置与模型配置共享同一个 Store 和同一把写锁。
    pub user_config: Arc<Mutex<MuseConfigStore>>,
    /// 串行化 Provider Profile 与活动模型的发布，避免删除和切换之间出现 TOCTOU。
    pub model_configuration_transition_gate: Mutex<()>,
    /// 运行时事实状态服务，独占当前会话、活动会话标识和单轮协调器。
    pub runtime_service: RuntimeService,
    pub personas: Mutex<PersonaStore>,
    pub visual_packs: Mutex<VisualPackStore>,
    /// 串行化角色事实、运行时会话切换与聊天 turn 占位，消除跨存储 TOCTOU。
    pub persona_runtime_transition_gate: Mutex<()>,
    pub tools: ToolRegistry,
    /// 可变工具串行门控，避免多个写状态工具并发修改工作区或运行时状态。
    pub mutating_tool_gate: Mutex<()>,
    /// 已受理流式聊天请求 ID，阻止网络重试重复创建有副作用的回合。
    pub chat_request_ids: Mutex<ChatRequestRegistry>,
    /// 用于情绪广播的 WebSocket 频道。
    pub emotion_tx: broadcast::Sender<String>,
}

#[derive(serde::Deserialize)]
struct RuntimePromptHarnessConfig {
    #[serde(default)]
    roots: Vec<String>,
    #[serde(default = "default_runtime_prompt_permission_mode")]
    permission_mode: String,
    #[serde(default = "default_runtime_prompt_sandbox_mode")]
    sandbox_mode: String,
}

fn default_runtime_prompt_permission_mode() -> String {
    "request_approval".to_string()
}

fn default_runtime_prompt_sandbox_mode() -> String {
    "workspace_write".to_string()
}

fn runtime_prompt_harness_config_path() -> PathBuf {
    Config::resolved_data_dir()
        .unwrap_or_else(|_| PathBuf::from(".muse"))
        .join("harness")
        .join("allowed-file-roots.json")
}

fn load_runtime_prompt_harness_config() -> RuntimePromptHarnessConfig {
    let Ok(content) = std::fs::read_to_string(runtime_prompt_harness_config_path()) else {
        return RuntimePromptHarnessConfig {
            roots: Vec::new(),
            permission_mode: default_runtime_prompt_permission_mode(),
            sandbox_mode: default_runtime_prompt_sandbox_mode(),
        };
    };
    serde_json::from_str(&content).unwrap_or(RuntimePromptHarnessConfig {
        roots: Vec::new(),
        permission_mode: default_runtime_prompt_permission_mode(),
        sandbox_mode: default_runtime_prompt_sandbox_mode(),
    })
}

fn initial_runtime_execution_policy() -> FrozenExecutionPolicy {
    let mut harness = load_runtime_prompt_harness_config();
    if !matches!(
        harness.permission_mode.as_str(),
        "request_approval" | "approve_for_me" | "full_access"
    ) {
        harness.permission_mode = default_runtime_prompt_permission_mode();
    }
    if !matches!(
        harness.sandbox_mode.as_str(),
        "workspace_write" | "danger_full_access"
    ) {
        harness.sandbox_mode = default_runtime_prompt_sandbox_mode();
    }
    if harness.permission_mode == "full_access" {
        // YOLO 只在当前进程的当前会话有效，禁止从旧全局配置静默恢复。
        harness.permission_mode = default_runtime_prompt_permission_mode();
        harness.sandbox_mode = default_runtime_prompt_sandbox_mode();
    }
    let roots = std::env::current_dir()
        .ok()
        .and_then(|root| root.canonicalize().ok())
        .into_iter()
        .collect();
    FrozenExecutionPolicy::new(harness.permission_mode, harness.sandbox_mode, roots)
}

fn runtime_prompt_permission_mode_label(value: &str) -> &'static str {
    match value {
        "approve_for_me" => "AUTO 模式",
        "full_access" => "完全访问权限",
        _ => "审批模式",
    }
}

fn runtime_prompt_sandbox_mode_label(value: &str) -> &'static str {
    match value {
        "danger_full_access" => "完全访问",
        _ => "工作区写入",
    }
}

fn runtime_prompt_home_dir() -> Option<PathBuf> {
    crate::platform::path::home_dir()
}

fn build_runtime_environment_context() -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut harness = load_runtime_prompt_harness_config();
    if harness.permission_mode == "full_access" {
        harness.permission_mode = default_runtime_prompt_permission_mode();
        harness.sandbox_mode = default_runtime_prompt_sandbox_mode();
    }

    let _legacy_allowed_roots = &harness.roots;

    let home = runtime_prompt_home_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "未知".to_string());

    format!(
        "【运行环境上下文】\n当前项目工作区：{}\n新会话默认审批：{}（{}）\n新会话默认沙箱：{}（{}）\n用户 Home：{}，可用 `~` 表示。\n{}\n\n【路径定位规则】\n相对路径默认按当前项目工作区解析。用户给出绝对路径或 `~` 路径时，先按原路径解析，再由 harness 判断是否需要审批。用户用“下载文件夹、桌面、文稿、项目目录”等自然语言描述路径时，先基于当前工作区、用户 Home、常见用户目录候选和消息上下文解析成最小候选路径；候选唯一时直接调用具体目标工具；候选多个时按 Codex 风格选择最像用户意图的路径：当前工作区优先，其次 Home 下浅层的非隐藏用户目录，再其次用户明确提到的隐藏目录或配置目录，并在最终回复中说明选择依据；只有候选含义完全等价或缺少文件名时再向用户澄清。不要为了确认权限或寻找用户目录去列出磁盘根、用户根目录、Home 根目录等宽泛目录；当前会话的真实审批与权限以本轮冻结快照为准。旧版“文件工具允许目录”白名单已停用，外部路径由单次操作审批或当前会话 YOLO 决定。",
        cwd.display(),
        runtime_prompt_permission_mode_label(&harness.permission_mode),
        harness.permission_mode,
        runtime_prompt_sandbox_mode_label(&harness.sandbox_mode),
        harness.sandbox_mode,
        home,
        crate::platform::path::known_user_directories_prompt()
    )
}

/// 构建当前运行时系统提示词。
pub fn build_runtime_system_prompt(
    config: &Config,
    tools: &ToolRegistry,
    active_persona: Option<&Persona>,
) -> String {
    let mode_state = RuntimeModeState::default();
    let tool_defs = tools.list_definitions_for_preset_and_policy(
        mode_state.tool_preset(),
        active_persona.map(|persona| &persona.tool_policy),
    );
    build_runtime_system_prompt_with_mode_state(config, active_persona, &tool_defs, mode_state)
}

/// 使用已合并的工具定义构建当前运行时系统提示词。
pub fn build_runtime_system_prompt_with_tool_defs(
    config: &Config,
    active_persona: Option<&Persona>,
    tool_defs: &[ToolDef],
) -> String {
    build_runtime_system_prompt_with_mode_state(
        config,
        active_persona,
        tool_defs,
        RuntimeModeState::default(),
    )
}

/// 使用已合并工具定义和运行模式构建当前运行时系统提示词。
pub fn build_runtime_system_prompt_with_mode_state(
    config: &Config,
    active_persona: Option<&Persona>,
    tool_defs: &[ToolDef],
    mode_state: RuntimeModeState,
) -> String {
    let mut system_prompt = match active_persona {
        Some(persona) => config.agent.build_persona_system_prompt(persona),
        None => NO_ACTIVE_PERSONA_SYSTEM_PROMPT.to_string(),
    };

    system_prompt.push_str("\n\n");
    system_prompt.push_str(&build_runtime_mode_context(mode_state));
    system_prompt.push_str("\n\n");
    system_prompt.push_str(&build_runtime_environment_context());

    let tools_prompt = ToolRegistry::tools_prompt_from_definitions(tool_defs);
    if !tools_prompt.is_empty() {
        system_prompt.push_str("\n\n");
        system_prompt.push_str(&tools_prompt);
    }

    system_prompt
}

fn build_runtime_mode_context(mode_state: RuntimeModeState) -> String {
    match mode_state.tool_preset() {
        muse_core::domain::runtime::ToolPreset::Daily => {
            "【运行状态】\n当前读取到旧版兼容工具预设。日常模式已经移除；后续新回合应使用默认工作态，复杂或有副作用的动作继续遵守审批、沙箱和工作区边界。".to_string()
        }
        muse_core::domain::runtime::ToolPreset::FocusPlan => {
            "【运行状态】\n当前状态：计划态。\n目标：围绕用户目标读文件、查资料、梳理任务和提出计划。计划态只能使用读查问和只读 MCP 资源，不直接改代码、不执行命令、不触发写入动作。计划态是临时只读收窄，不是默认工作流程的必经阶段。".to_string()
        }
        muse_core::domain::runtime::ToolPreset::FocusBuild => {
            "【运行状态】\n当前状态：默认工作态。\n目标：根据用户意图使用全部已授权通用工具、Skill 与角色资源，可在审批边界内执行编辑、命令、MCP 工具和验证。需要先规划时再临时进入计划态；遇到审批、风险或计划外变更时先停下说明。".to_string()
        }
    }
}

/// 构建网页适配层共享状态，并完成运行时配置、资源和角色展示初始化。
pub fn build_app_state(config: Config) -> Result<Arc<AppState>, Box<dyn std::error::Error>> {
    let data_dir = Config::config_dir();
    let mut user_config = MuseConfigStore::load_from_dir(&data_dir)?;
    let mut legacy_model_config = ModelConfigStore::load_from_dir(&data_dir)?;
    let secrets = PlatformSecretStore::new("Muse");
    migrate_model_profiles(
        &data_dir,
        &mut user_config,
        &mut legacy_model_config,
        &secrets,
    )?;
    migrate_mcp_profiles(
        &data_dir,
        &mut user_config,
        legacy_model_config.mcp_servers(),
        &secrets,
    )?;
    migrate_legacy_skill_enabled(&data_dir, &mut user_config)?;
    // Provider Profile 已发布后才允许数据库 migration 删除旧 providers/models 表。
    let _ = muse_core::app::storage::open_runtime_database(&data_dir)?;
    let provider = build_chat_provider(user_config.chat())?;
    let tts_provider = build_tts_provider(user_config.tts());
    let speech_recognition_provider =
        build_speech_recognition_provider(user_config.speech_recognition());
    let personas = load_runtime_personas(&data_dir)?;
    let visual_packs = VisualPackStore::load_from_dir(Config::config_dir())?;

    let mut tools = ToolRegistry::new();
    builtin::register_all(&mut tools);

    let system_prompt = build_runtime_system_prompt(&config, &tools, personas.active_persona());
    let conversation = Conversation::new(system_prompt, config.agent.max_history);
    let (emotion_tx, _) = broadcast::channel::<String>(32);

    let shared_config = Arc::new(Mutex::new(user_config));
    Ok(Arc::new(AppState {
        config,
        secrets,
        provider: Mutex::new(provider),
        tts_provider: Mutex::new(tts_provider),
        speech_recognition_provider: Mutex::new(speech_recognition_provider),
        model_config: Arc::clone(&shared_config),
        user_config: shared_config,
        model_configuration_transition_gate: Mutex::new(()),
        runtime_service: RuntimeService::new_with_data_dir(
            next_runtime_session_id(),
            conversation,
            data_dir,
        )
        .with_execution_policy(initial_runtime_execution_policy()),
        personas: Mutex::new(personas),
        visual_packs: Mutex::new(visual_packs),
        persona_runtime_transition_gate: Mutex::new(()),
        tools,
        mutating_tool_gate: Mutex::new(()),
        chat_request_ids: Mutex::new(ChatRequestRegistry::load_from_dir(Config::config_dir())?),
        emotion_tx,
    }))
}

/// 加载角色事实状态；空库和未激活状态都必须原样保留。
fn load_runtime_personas(data_dir: &Path) -> Result<PersonaStore, PersonaStoreError> {
    PersonaStore::load_from_dir(data_dir)
}

/// 根据聊天配置构建真实模型 provider；未配置时返回空值，聊天路径会给出明确配置错误。
pub fn build_chat_provider(
    chat: &LlmConfig,
) -> Result<Option<Arc<dyn ChatModelProvider>>, ChatModelError> {
    if !chat.enabled() {
        tracing::warn!("聊天模型未配置，聊天请求会返回配置错误。");
        return Ok(None);
    }

    let boxed = match factory::create_provider(chat) {
        Ok(provider) => provider,
        Err(ChatModelError::ConfigError(message)) => {
            tracing::warn!("{message} 旧配置将只读保留，聊天请求会返回配置错误。");
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let provider: Arc<dyn ChatModelProvider> = Arc::from(boxed);
    Ok(Some(provider))
}

/// 构造聊天模型未配置错误，供聊天、compact 和模型状态路径统一返回。
pub fn missing_chat_provider_error() -> ChatModelError {
    ChatModelError::ConfigError(
        "聊天模型未配置，请先在设置页填写真实模型 provider、API 地址、模型名和密钥。".to_string(),
    )
}

/// 根据语音合成配置构建提供器；未启用、配置不完整或失败时返回空值。
pub fn build_tts_provider(tts: &TtsConfig) -> Option<Arc<dyn TtsProvider>> {
    let effective = effective_tts_config(tts);
    if !effective.enabled() {
        return None;
    }
    if effective.is_external_provider() {
        return match muse_core::speech::factory::create_tts_provider(&effective) {
            Ok(Some(boxed)) => Some(Arc::from(boxed)),
            Ok(None) => None,
            Err(err) => {
                tracing::warn!("外接 TTS provider 构建失败：{err}");
                None
            }
        };
    }
    tracing::warn!("TTS provider 已停用内置本地运行时，请配置外接语音服务。");
    None
}

/// 根据远程语音识别配置构建识别器；未启用或失败时返回空值。
pub fn build_speech_recognition_provider(
    speech_recognition: &SpeechRecognitionConfig,
) -> Option<Arc<dyn SpeechRecognitionProvider>> {
    if !speech_recognition.enabled() {
        return None;
    }
    match muse_core::speech::factory::create_speech_recognition_provider(speech_recognition) {
        Ok(Some(boxed)) => Some(Arc::from(boxed)),
        Ok(None) => None,
        Err(err) => {
            tracing::warn!("语音识别服务构建失败：{err}");
            None
        }
    }
}

/// 根据当前共享状态重建语音合成提供器。
pub async fn rebuild_tts_provider_from_state(state: &Arc<AppState>) {
    let tts = {
        let config = state.model_config.lock().await;
        config.tts().clone()
    };
    *state.tts_provider.lock().await = build_tts_provider(&tts);
}

/// 根据当前共享状态重建语音识别提供器。
pub async fn rebuild_speech_provider_from_state(state: &Arc<AppState>) {
    let speech_recognition = {
        let config = state.model_config.lock().await;
        config.speech_recognition().clone()
    };
    *state.speech_recognition_provider.lock().await =
        build_speech_recognition_provider(&speech_recognition);
}

/// 计算实际用于语音合成运行时的配置。
pub(crate) fn effective_tts_config(tts: &TtsConfig) -> TtsConfig {
    tts.clone()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use muse_core::config::Config;
    use muse_core::domain::persona::character::store::PersonaStore;
    use muse_core::domain::persona::{Persona, RoleplayStyle, ToolPolicy};
    use muse_core::domain::tool::ToolRegistry;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "muse-state-{prefix}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&dir).expect("应能创建测试临时目录");
        dir
    }

    #[test]
    fn new_runtime_session_id_does_not_reuse_legacy_default() {
        let session_id = super::next_runtime_session_id();

        assert!(session_id.starts_with("session-"));
        assert_ne!(session_id, "default");
    }

    fn test_persona(id: &str) -> Persona {
        Persona {
            id: id.to_string(),
            name: "用户角色".to_string(),
            summary: "测试角色".to_string(),
            character_profile: "冷静、可靠".to_string(),
            world_profile: "现实日常".to_string(),
            scenario: String::new(),
            system_prompt: "保持用户保存的角色设定。".to_string(),
            style: "简洁".to_string(),
            roleplay_style: RoleplayStyle::LightNarration,
            dialogue_examples: String::new(),
            author_note: String::new(),
            opening_message: String::new(),
            tool_policy: ToolPolicy::default(),
            skill_policy: Default::default(),
            mcp_policy: Default::default(),
            preferred_model_ref: None,
            preferred_voice_id: None,
            feature_policy: Default::default(),
            default_visual_pack_id: "default-visual-pack".to_string(),
            author: "user".to_string(),
            version: "1.0.0".to_string(),
            notes: String::new(),
        }
    }

    #[test]
    fn runtime_persona_loading_preserves_empty_library() {
        let dir = unique_temp_dir("empty-persona-library");

        let store = super::load_runtime_personas(&dir).expect("空角色库应能正常加载");

        assert!(store.personas().is_empty());
        assert!(store.active_persona_id().is_none());
        assert!(
            !dir.join("personas/personas.json").exists(),
            "空启动不得为默认角色创建存储文件"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn runtime_persona_loading_does_not_activate_first_persona() {
        let dir = unique_temp_dir("inactive-persona-library");
        let mut store = PersonaStore::load_from_dir(&dir).expect("应能创建角色库");
        store
            .create(test_persona("persona-a"))
            .expect("应能创建测试角色");
        store.save().expect("应能保存未激活角色库");

        let reloaded = super::load_runtime_personas(&dir).expect("应能重新加载角色库");

        assert_eq!(reloaded.personas().len(), 1);
        assert!(reloaded.active_persona_id().is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn runtime_persona_loading_preserves_existing_default_persona_data() {
        let dir = unique_temp_dir("existing-default-persona");
        let mut store = PersonaStore::load_from_dir(&dir).expect("应能创建角色库");
        let persona = test_persona("default-persona");
        store.create(persona.clone()).expect("应能创建旧角色");
        store
            .set_active(&persona.id)
            .expect("应能保留旧角色激活状态");
        store.save().expect("应能保存旧角色数据");

        let reloaded = super::load_runtime_personas(&dir).expect("应能重新加载旧角色数据");

        assert_eq!(reloaded.get(&persona.id), Some(&persona));
        assert_eq!(reloaded.active_persona_id(), Some(persona.id.as_str()));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn no_active_persona_uses_neutral_runtime_prompt() {
        let config = Config::default();
        let prompt = super::build_runtime_system_prompt(&config, &ToolRegistry::new(), None);

        assert!(prompt.contains("当前没有激活角色"));
        assert!(!prompt.contains(&config.character.name));
        assert!(!prompt.contains("小灵"));
    }

    #[test]
    fn chat_request_registry_refuses_capacity_instead_of_arbitrary_eviction() {
        let mut registry = super::ChatRequestRegistry::with_limits(None, 2, u64::MAX);
        registry.accept("request-a").expect("首个请求应受理");
        registry.accept("request-b").expect("第二个请求应受理");

        assert_eq!(
            registry.accept("request-c"),
            Err(super::ChatRequestRegistryError::CapacityExceeded)
        );
        assert_eq!(
            registry.accept("request-a"),
            Err(super::ChatRequestRegistryError::Duplicate),
            "容量耗尽不能通过任意删除旧 ID 让重放请求重新进入"
        );
    }

    #[test]
    fn chat_request_terminal_state_survives_registry_reload() {
        let dir = unique_temp_dir("chat-request-registry");
        {
            let mut registry =
                super::ChatRequestRegistry::load_from_dir(dir.clone()).expect("应能创建登记表");
            registry
                .accept("durable-request")
                .expect("请求受理事实应持久化");
            registry
                .mark_terminal("durable-request")
                .expect("请求终态应持久化");
        }

        let mut reloaded =
            super::ChatRequestRegistry::load_from_dir(dir.clone()).expect("应能重载登记表");
        assert_eq!(
            reloaded.accept("durable-request"),
            Err(super::ChatRequestRegistryError::Duplicate)
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_chat_provider_does_not_block_startup_or_enter_runtime() {
        let legacy = muse_core::model::config::LlmConfig {
            provider: "volcengine_ark".to_string(),
            api_base: "https://ark.cn-beijing.volces.com/api/v3".to_string(),
            model: "doubao-seed-2-0-lite-260215".to_string(),
            ..Default::default()
        };

        assert!(
            super::build_chat_provider(&legacy)
                .expect("旧配置不应阻止应用启动")
                .is_none(),
            "停止支持的 Provider 不得进入聊天运行时"
        );
    }
}
