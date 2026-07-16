//! 应用启动配置模块，保留服务端口、智能体提示词与角色兜底配置。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::domain::persona::{Persona, RoleplayStyle};

const LEGACY_MIGRATION_MARKER: &str = ".muse-migrations/legacy-workspace-v1.json";
static MIGRATION_STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// 数据目录解析或 legacy 数据迁移失败。
#[derive(Debug, Clone)]
pub struct DataDirError {
    message: String,
}

impl DataDirError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DataDirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DataDirError {}

/// 桌面端首次启动时的工作区 legacy 数据迁移报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyWorkspaceMigrationReport {
    /// 被复制并转换为只读外部引用的模型资产数量。
    pub external_model_assets: usize,
    /// 被改写为 legacy 绝对路径的模型配置路径数量。
    pub external_model_paths: usize,
    /// legacy 目录中是否存在未复制的 `model-files`。
    pub excluded_model_files: bool,
}

/// 与数据目录路径绑定的跨进程排他锁。
///
/// 锁文件放在当前用户的临时锁注册表中，不会提前创建目标数据目录，因此首次迁移仍可
/// 使用 staging 目录原子替换目标。桌面端和 legacy 源目录统一使用此协议。
#[derive(Debug)]
pub struct DataDirLock {
    _file: File,
    lock_path: PathBuf,
}

impl DataDirLock {
    /// 返回实际锁文件位置，仅用于诊断和测试，不代表数据存储位置。
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }
}

#[derive(Debug, Serialize)]
struct LegacyWorkspaceMigrationMarker {
    schema_version: u32,
    source_dir: String,
    migrated_at: String,
    external_model_assets: usize,
    external_model_paths: usize,
    excluded_model_files: bool,
}

/// 应用顶层启动配置。
///
/// 仅承载启动级配置（服务器端口、agent 提示词、角色兜底）。
/// 模型类配置（聊天、语音合成、语音识别）由用户数据目录的
/// `config.toml` Provider Profile 统一持久化，并支持热更新。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 网页服务设置。
    pub server: ServerConfig,
    /// 智能体角色与系统提示词设置。
    pub agent: AgentConfig,
    /// 虚拟角色与角色兜底设置。
    #[serde(default)]
    pub character: CharacterConfig,
}

/// 角色兜底配置，用于没有激活角色时渲染系统提示词。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterConfig {
    /// 角色名称。
    #[serde(default = "default_character_name")]
    pub name: String,
    /// 性格描述（如"活泼、好奇心强、偶尔毒舌"）。
    #[serde(default)]
    pub personality: String,
    /// 说话风格提示（如"使用口语化中文，回复控制在2-3句话"）。
    #[serde(default)]
    pub speech_style: String,
    /// 是否禁用情绪识别。
    #[serde(default)]
    pub emotion_disabled: bool,
}

fn default_character_name() -> String {
    "小灵".into()
}

impl Default for CharacterConfig {
    fn default() -> Self {
        Self {
            name: default_character_name(),
            personality: String::new(),
            speech_style: String::new(),
            emotion_disabled: false,
        }
    }
}

/// 网页服务监听配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

/// 智能体基础提示词配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// 系统提示词模板，支持占位符 {name} {personality} {speech_style}。
    pub system_prompt: String,
    /// 上下文中保留的最大会话轮数。
    pub max_history: usize,
}

impl AgentConfig {
    /// 根据通用字段渲染完整系统提示词。
    fn render_prompt(
        &self,
        name: &str,
        personality: &str,
        speech_style: &str,
        emotion_disabled: bool,
    ) -> String {
        let mut prompt = self
            .system_prompt
            .replace("{name}", name)
            .replace("{personality}", personality)
            .replace("{speech_style}", speech_style);

        if !emotion_disabled {
            prompt.push_str("\n\n");
            prompt.push_str("【情绪输出规则】在每条回复的最开头，用一行 JSON 标明你的情绪，格式为 {\"emotion\":\"happy|sad|surprised|thinking|neutral|angry\"}，然后换行再写正常回复。示例：\n{\"emotion\":\"happy\"}\n你好呀！");
        }

        prompt
    }

    /// 根据角色配置渲染完整系统提示词。
    pub fn build_system_prompt(&self, character: &CharacterConfig) -> String {
        self.render_prompt(
            &character.name,
            &character.personality,
            &character.speech_style,
            character.emotion_disabled,
        )
    }

    /// 根据角色资产渲染完整系统提示词。
    pub fn build_persona_system_prompt(&self, persona: &Persona) -> String {
        let mut prompt = self.render_prompt(
            &persona.name,
            &persona.character_profile,
            &persona.style,
            false,
        );

        prompt.push_str("\n\n【当前角色资料】\n");
        prompt.push_str(&format!("名称：{}\n", persona.name.trim()));
        if !persona.character_profile.trim().is_empty() {
            prompt.push_str(&format!("角色特征：{}\n", persona.character_profile.trim()));
        }
        if !persona.style.trim().is_empty() {
            prompt.push_str(&format!("说话风格：{}\n", persona.style.trim()));
        }

        if !persona.world_profile.trim().is_empty() {
            prompt.push_str("\n\n【世界观设定】\n");
            prompt.push_str(persona.world_profile.trim());
        }

        if !persona.scenario.trim().is_empty() {
            prompt.push_str("\n\n【当前场景】\n");
            prompt.push_str(persona.scenario.trim());
        } else {
            prompt.push_str("\n\n【默认场景】\n");
            prompt.push_str(
                "未配置当前场景时，仍默认处在与用户面对面的日常互动中；普通问候也要体现角色在场的动作、表情和一句自然对白，不要只做说明式回应。",
            );
        }

        if !persona.system_prompt.trim().is_empty() {
            prompt.push_str("\n\n【角色补充设定】\n");
            prompt.push_str(persona.system_prompt.trim());
        }

        prompt.push_str("\n\n");
        prompt.push_str(roleplay_style_prompt(persona.roleplay_style));

        if !persona.dialogue_examples.trim().is_empty() {
            prompt.push_str("\n\n【演绎示例】\n");
            prompt.push_str(persona.dialogue_examples.trim());
        }

        if !persona.author_note.trim().is_empty() {
            prompt.push_str("\n\n【演绎备注】\n");
            prompt.push_str(persona.author_note.trim());
        }

        prompt
    }
}

fn roleplay_style_prompt(style: RoleplayStyle) -> &'static str {
    match style {
        RoleplayStyle::Dialogue => {
            "【角色演绎规则】\n在普通寒暄、情感陪伴、角色扮演或虚构场景中，默认保持角色在场并以角色对白回应；即使用户只说“你好”，也要给出符合当前角色的一句自然回应，可少量加入动作或表情描写。不要解释自己将如何扮演，不要替用户决定言行。遇到工具、设置、代码、事实问答或非剧情任务时，按任务需求正常回答。"
        }
        RoleplayStyle::LightNarration => {
            "【角色演绎规则】\n在普通寒暄、情感陪伴、角色扮演或虚构场景中，默认把回复写成“轻量场景感 + 角色动作/表情 + 对白”的自然段落；即使用户只说“你好”，也要先让角色以可感知的神态、姿态或语气出场，再说出有辨识度的回应。描写应服务于当前互动，不要替用户决定言行，不要一次性推进过多剧情。遇到工具、设置、代码、事实问答或非剧情任务时，按任务需求正常回答。"
        }
        RoleplayStyle::Immersive => {
            "【角色演绎规则】\n在普通寒暄、情感陪伴、角色扮演或虚构场景中，默认写出沉浸式回复：包含角色动作、情绪变化、环境细节、节奏停顿和必要的感官描写，并以角色对白推动互动；即使用户只说“你好”，也要把它当作角色互动的开场来回应。每次只写一轮回复，不替用户决定言行，不重复规则文本，不跳过当前场景的细节。遇到工具、设置、代码、事实问答或非剧情任务时，按任务需求正常回答。"
        }
        RoleplayStyle::TextAdventure => {
            "【角色演绎规则】\n在普通寒暄、情感陪伴、角色扮演、虚构场景或文字冒险中，默认以叙事主持人的方式描写周围环境、角色反应和可感知线索；即使用户只说“你好”，也要给出一个可继续选择或回应的场景开场。主动推进当前场景但保留用户选择空间，不要替用户决定关键行动或台词。遇到工具、设置、代码、事实问答或非剧情任务时，按任务需求正常回答。"
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                host: "127.0.0.1".into(),
                port: 3000,
            },
            agent: AgentConfig {
                system_prompt: "You are a helpful AI assistant with a friendly personality.".into(),
                max_history: 50,
            },
            character: CharacterConfig::default(),
        }
    }
}

impl Config {
    /// 加载当前内置启动配置。
    ///
    /// 当前启动级配置直接以内置默认值为准；运行时模型配置由用户数据目录的
    /// `config.toml` 承载。
    pub fn load() -> Self {
        Self::default()
    }

    /// 返回本地数据目录路径，具体位置与平台有关。
    ///
    /// 历史上这里叫 `config_dir`，但实际承载的是本地运行数据：
    /// 模型配置、模型文件、会话、日志、SQLite、MCP blob 等。为避免一次性牵动过大，
    /// 旧函数名暂时保留。
    pub fn config_dir() -> PathBuf {
        Self::try_config_dir().unwrap_or_else(|error| {
            panic!("无法初始化 Muse 数据目录：{error}");
        })
    }

    /// 按稳定优先级解析并创建桌面数据目录。
    ///
    /// 显式目录创建失败时直接返回错误，绝不静默切换到第二套数据目录。
    pub fn try_config_dir() -> Result<PathBuf, DataDirError> {
        let path = Self::resolved_data_dir()?;
        ensure_data_directory(&path, "Muse 用户数据目录")?;
        Ok(path)
    }

    /// 解析数据目录但不创建目录、不修改权限，供只读路径展示和探测使用。
    pub fn resolved_data_dir() -> Result<PathBuf, DataDirError> {
        if let Some(dir) = std::env::var_os("MUSE_DATA_DIR") {
            return Ok(PathBuf::from(dir));
        }

        Self::default_user_data_dir()
    }

    /// 返回桌面应用使用的用户主目录 `~/.muse`，但不提前创建目录。
    pub fn default_user_data_dir() -> Result<PathBuf, DataDirError> {
        directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().join(".muse"))
            .ok_or_else(|| {
                DataDirError::new("当前平台未提供可用的用户主目录，请通过 MUSE_DATA_DIR 显式指定。")
            })
    }

    /// 只把带完整项目标识的仓库根目录识别为 `.agent-vp-data` legacy 来源。
    pub fn discover_legacy_workspace_data(workspace: &Path) -> Option<PathBuf> {
        let legacy_dir = workspace.join(".agent-vp-data");
        let is_workspace_root = workspace.join("Cargo.toml").is_file()
            && (workspace.join("package.json").is_file()
                || workspace.join("persona-ui/package.json").is_file())
            && (workspace.join("src-tauri/Cargo.toml").is_file()
                || workspace.join("muse-core/Cargo.toml").is_file()
                || workspace.join("persona-core/Cargo.toml").is_file());
        (is_workspace_root && legacy_dir.is_dir()).then_some(legacy_dir)
    }

    /// 返回本地运行数据目录。语义上等同于历史 `config_dir()`。
    pub fn data_dir() -> PathBuf {
        Self::config_dir()
    }

    /// 将工作区 legacy 数据无损迁移到首次使用的 `~/.muse`。
    ///
    /// 迁移先在目标目录旁创建 staging 副本，完成模型引用改写和 marker 落盘后再原子
    /// 提交。目标目录只要出现任何已有文件就会跳过迁移，绝不覆盖现有安装。
    pub fn migrate_legacy_workspace_data(
        target_dir: impl AsRef<Path>,
        legacy_dir: impl AsRef<Path>,
    ) -> Result<Option<LegacyWorkspaceMigrationReport>, DataDirError> {
        migrate_legacy_workspace_data(target_dir.as_ref(), legacy_dir.as_ref())
    }

    /// 取得与指定数据目录绑定的跨进程排他锁。
    ///
    /// 该方法不会创建数据目录本身，允许调用方先锁定尚不存在的迁移目标。
    pub fn acquire_data_dir_lock(data_dir: impl AsRef<Path>) -> Result<DataDirLock, DataDirError> {
        acquire_data_dir_lock(data_dir.as_ref())
    }
}

fn acquire_data_dir_lock(data_dir: &Path) -> Result<DataDirLock, DataDirError> {
    let lock_path = data_dir_lock_path(data_dir)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let mut file = options.open(&lock_path).map_err(|error| {
        DataDirError::new(format!(
            "无法取得数据目录排他锁 `{}`：{error}",
            lock_path.display()
        ))
    })?;

    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let metadata = file
            .metadata()
            .map_err(|error| DataDirError::new(format!("无法检查数据目录锁文件：{error}")))?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(DataDirError::new(
                "数据目录锁文件必须是仅归当前用户所有的私有普通文件。",
            ));
        }
        // SAFETY: File 在 DataDirLock 生命周期内保持打开，flock 仅操作该有效描述符。
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err(DataDirError::new(format!(
                "数据目录 `{}` 正被另一个 Muse 进程使用。",
                data_dir.display()
            )));
        }
    }

    file.set_len(0)
        .and_then(|_| {
            writeln!(
                file,
                "pid={}\nstarted_at={}\ndata_dir={}",
                std::process::id(),
                chrono::Utc::now().to_rfc3339(),
                data_dir.display()
            )
        })
        .and_then(|_| file.sync_all())
        .map_err(|error| DataDirError::new(format!("写入数据目录锁信息失败：{error}")))?;

    Ok(DataDirLock {
        _file: file,
        lock_path,
    })
}

fn data_dir_lock_path(data_dir: &Path) -> Result<PathBuf, DataDirError> {
    if data_dir.as_os_str().is_empty() {
        return Err(DataDirError::new("数据目录锁不能绑定到空路径。"));
    }
    let absolute = if data_dir.is_absolute() {
        data_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| DataDirError::new(format!("无法解析数据目录绝对路径：{error}")))?
            .join(data_dir)
    };
    let identity = if absolute.exists() {
        let metadata = fs::symlink_metadata(&absolute).map_err(|error| {
            DataDirError::new(format!(
                "无法检查数据目录 `{}`：{error}",
                absolute.display()
            ))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(DataDirError::new(format!(
                "数据目录 `{}` 必须是真实目录。",
                absolute.display()
            )));
        }
        fs::canonicalize(&absolute).map_err(|error| {
            DataDirError::new(format!(
                "无法规范化数据目录 `{}`：{error}",
                absolute.display()
            ))
        })?
    } else {
        let parent = absolute.parent().ok_or_else(|| {
            DataDirError::new(format!("数据目录 `{}` 缺少父目录。", absolute.display()))
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            DataDirError::new(format!(
                "无法创建数据目录父目录 `{}`：{error}",
                parent.display()
            ))
        })?;
        let canonical_parent = fs::canonicalize(parent).map_err(|error| {
            DataDirError::new(format!(
                "无法规范化数据目录父目录 `{}`：{error}",
                parent.display()
            ))
        })?;
        let name = absolute.file_name().ok_or_else(|| {
            DataDirError::new(format!("数据目录 `{}` 缺少目录名称。", absolute.display()))
        })?;
        canonical_parent.join(name)
    };

    let lock_root = data_dir_lock_root()?;
    let normalized = identity.to_string_lossy().to_lowercase();
    let digest = Sha256::digest(normalized.as_bytes());
    let mut file_name = String::with_capacity(digest.len() * 2 + 5);
    file_name.push_str("lock-");
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        file_name.push(HEX[(byte >> 4) as usize] as char);
        file_name.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(lock_root.join(file_name))
}

fn data_dir_lock_root() -> Result<PathBuf, DataDirError> {
    #[cfg(unix)]
    let root = std::env::temp_dir().join(format!(
        "muse-data-locks-v1-{}",
        // SAFETY: geteuid 没有前置条件，只读取当前进程的有效用户 ID。
        unsafe { libc::geteuid() }
    ));
    #[cfg(windows)]
    let root = directories::ProjectDirs::from("com", "muse", "runtime-locks")
        .ok_or_else(|| DataDirError::new("无法定位当前 Windows 用户的私有锁目录。"))?
        .data_local_dir()
        .join("locks-v1");
    #[cfg(not(any(unix, windows)))]
    let root = std::env::temp_dir().join("muse-data-locks-v1");
    match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(DataDirError::new(format!(
                "数据目录锁注册表 `{}` 不是可信目录。",
                root.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = fs::DirBuilder::new();
                builder.mode(0o700);
                if let Err(error) = builder.create(&root)
                    && error.kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(DataDirError::new(format!(
                        "无法创建数据目录锁注册表 `{}`：{error}",
                        root.display()
                    )));
                }
            }
            #[cfg(not(unix))]
            if let Err(error) = fs::create_dir_all(&root) {
                return Err(DataDirError::new(format!(
                    "无法创建数据目录锁注册表 `{}`：{error}",
                    root.display()
                )));
            }
        }
        Err(error) => {
            return Err(DataDirError::new(format!(
                "无法检查数据目录锁注册表 `{}`：{error}",
                root.display()
            )));
        }
    }

    let verified = fs::symlink_metadata(&root)
        .map_err(|error| DataDirError::new(format!("无法复核数据目录锁注册表：{error}")))?;
    if verified.file_type().is_symlink() || !verified.is_dir() {
        return Err(DataDirError::new(
            "数据目录锁注册表必须是真实目录，不能是符号链接。",
        ));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if verified.uid() != unsafe { libc::geteuid() }
            || verified.permissions().mode() & 0o077 != 0
        {
            return Err(DataDirError::new(
                "数据目录锁注册表必须是仅归当前用户所有的私有真实目录。",
            ));
        }
    }
    Ok(root)
}

fn ensure_data_directory(path: &Path, source: &str) -> Result<(), DataDirError> {
    if path.as_os_str().is_empty() {
        return Err(DataDirError::new(format!(
            "{source} 指向空路径，拒绝继续启动。"
        )));
    }

    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() => Err(DataDirError::new(format!(
            "{source} 指向的路径 `{}` 不是目录。",
            path.display()
        ))),
        Ok(_) => make_data_directory_private(path, source),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir_all(path)
            .map_err(|error| {
                DataDirError::new(format!("无法创建 {source} `{}`：{error}", path.display()))
            })
            .and_then(|_| make_data_directory_private(path, source)),
        Err(error) => Err(DataDirError::new(format!(
            "无法检查 {source} `{}`：{error}",
            path.display()
        ))),
    }
}

fn make_data_directory_private(path: &Path, source: &str) -> Result<(), DataDirError> {
    #[cfg(not(unix))]
    let _ = (path, source);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            DataDirError::new(format!(
                "无法收紧 {source} `{}` 的访问权限：{error}",
                path.display()
            ))
        })?;
    }
    Ok(())
}

fn migrate_legacy_workspace_data(
    target_dir: &Path,
    legacy_dir: &Path,
) -> Result<Option<LegacyWorkspaceMigrationReport>, DataDirError> {
    let legacy_metadata = match fs::symlink_metadata(legacy_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ensure_data_directory(target_dir, "桌面应用数据目录")?;
            return Ok(None);
        }
        Err(error) => {
            return Err(DataDirError::new(format!(
                "无法检查 legacy 数据目录 `{}`：{error}",
                legacy_dir.display()
            )));
        }
    };
    if legacy_metadata.file_type().is_symlink() || !legacy_metadata.is_dir() {
        return Err(DataDirError::new(format!(
            "legacy 数据路径 `{}` 必须是真实目录，不能是文件或符号链接。",
            legacy_dir.display()
        )));
    }

    let canonical_legacy = fs::canonicalize(legacy_dir).map_err(|error| {
        DataDirError::new(format!(
            "无法规范化 legacy 数据目录 `{}`：{error}",
            legacy_dir.display()
        ))
    })?;

    if target_dir.exists() {
        let metadata = fs::symlink_metadata(target_dir).map_err(|error| {
            DataDirError::new(format!(
                "无法检查桌面应用数据目录 `{}`：{error}",
                target_dir.display()
            ))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(DataDirError::new(format!(
                "桌面应用数据路径 `{}` 必须是真实目录。",
                target_dir.display()
            )));
        }
        if fs::canonicalize(target_dir).ok().as_deref() == Some(canonical_legacy.as_path()) {
            return Ok(None);
        }
        if !directory_is_empty(target_dir)? {
            return Ok(None);
        }
    }

    let target_parent = target_dir.parent().ok_or_else(|| {
        DataDirError::new(format!(
            "桌面应用数据目录 `{}` 缺少父目录。",
            target_dir.display()
        ))
    })?;
    fs::create_dir_all(target_parent).map_err(|error| {
        DataDirError::new(format!(
            "无法创建桌面应用数据父目录 `{}`：{error}",
            target_parent.display()
        ))
    })?;
    let canonical_target_parent = fs::canonicalize(target_parent).map_err(|error| {
        DataDirError::new(format!(
            "无法规范化桌面应用数据父目录 `{}`：{error}",
            target_parent.display()
        ))
    })?;
    if canonical_target_parent.starts_with(&canonical_legacy) {
        return Err(DataDirError::new(format!(
            "桌面应用数据目录 `{}` 不能位于 legacy 数据目录内部。",
            target_dir.display()
        )));
    }

    let target_name = target_dir
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("muse-data");
    let sequence = MIGRATION_STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let staging_dir = target_parent.join(format!(
        ".{target_name}.legacy-migration-{}-{sequence}.tmp",
        std::process::id()
    ));
    fs::create_dir(&staging_dir).map_err(|error| {
        DataDirError::new(format!(
            "无法创建 legacy 迁移暂存目录 `{}`：{error}",
            staging_dir.display()
        ))
    })?;

    let migration_result = build_legacy_migration_staging(&canonical_legacy, &staging_dir);
    let report = match migration_result {
        Ok(report) => report,
        Err(error) => {
            let cleanup_error = fs::remove_dir_all(&staging_dir).err();
            return Err(match cleanup_error {
                Some(cleanup_error) => DataDirError::new(format!(
                    "{error}；同时清理迁移暂存目录失败：{cleanup_error}"
                )),
                None => error,
            });
        }
    };

    if target_dir.exists() {
        if !directory_is_empty(target_dir)? {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(DataDirError::new(format!(
                "迁移期间目标目录 `{}` 出现了已有文件，已拒绝覆盖。",
                target_dir.display()
            )));
        }
        fs::remove_dir(target_dir).map_err(|error| {
            let _ = fs::remove_dir_all(&staging_dir);
            DataDirError::new(format!(
                "无法提交 legacy 迁移，目标空目录 `{}` 无法替换：{error}",
                target_dir.display()
            ))
        })?;
    }

    if let Err(error) = fs::rename(&staging_dir, target_dir) {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(DataDirError::new(format!(
            "无法原子提交 legacy 数据到 `{}`：{error}",
            target_dir.display()
        )));
    }

    Ok(Some(report))
}

fn directory_is_empty(path: &Path) -> Result<bool, DataDirError> {
    let mut entries = fs::read_dir(path).map_err(|error| {
        DataDirError::new(format!("无法读取目录 `{}`：{error}", path.display()))
    })?;
    Ok(entries.next().is_none())
}

fn build_legacy_migration_staging(
    canonical_legacy: &Path,
    staging_dir: &Path,
) -> Result<LegacyWorkspaceMigrationReport, DataDirError> {
    copy_legacy_tree(canonical_legacy, staging_dir)?;
    let excluded_model_files = canonical_legacy.join("model-files").exists()
        || canonical_legacy.join("models/assets.json").exists();
    let external_model_assets = 0;
    let external_model_paths = 0;
    let source_dir = canonical_legacy.to_str().ok_or_else(|| {
        DataDirError::new("legacy 数据目录包含无法写入 JSON marker 的非 Unicode 字符。")
    })?;
    let marker = LegacyWorkspaceMigrationMarker {
        schema_version: 1,
        source_dir: source_dir.to_string(),
        migrated_at: chrono::Utc::now().to_rfc3339(),
        external_model_assets,
        external_model_paths,
        excluded_model_files,
    };
    write_json_file(
        &staging_dir.join(LEGACY_MIGRATION_MARKER),
        &serde_json::to_value(marker).map_err(|error| {
            DataDirError::new(format!("无法序列化 legacy 迁移 marker：{error}"))
        })?,
    )?;
    apply_legacy_permissions(canonical_legacy, staging_dir, true)?;
    make_marker_private(&staging_dir.join(LEGACY_MIGRATION_MARKER))?;

    Ok(LegacyWorkspaceMigrationReport {
        external_model_assets,
        external_model_paths,
        excluded_model_files,
    })
}

fn copy_legacy_tree(source: &Path, destination: &Path) -> Result<(), DataDirError> {
    copy_legacy_tree_filtered(source, destination, source)
}

fn copy_legacy_tree_filtered(
    source: &Path,
    destination: &Path,
    legacy_root: &Path,
) -> Result<(), DataDirError> {
    let mut entries = fs::read_dir(source)
        .map_err(|error| {
            DataDirError::new(format!(
                "无法读取 legacy 目录 `{}`：{error}",
                source.display()
            ))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            DataDirError::new(format!(
                "无法枚举 legacy 目录 `{}`：{error}",
                source.display()
            ))
        })?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let source_path = entry.path();
        let relative = source_path
            .strip_prefix(legacy_root)
            .map_err(|error| DataDirError::new(format!("无法解析 legacy 相对路径：{error}")))?;
        if relative.starts_with("model-files") || relative == Path::new("models/assets.json") {
            continue;
        }
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type().map_err(|error| {
            DataDirError::new(format!(
                "无法检查 legacy 项 `{}`：{error}",
                source_path.display()
            ))
        })?;
        if file_type.is_symlink() {
            return Err(DataDirError::new(format!(
                "legacy 数据包含符号链接 `{}`，为避免复制目录外数据已中止迁移。",
                source_path.display()
            )));
        }
        if file_type.is_dir() {
            fs::create_dir(&destination_path).map_err(|error| {
                DataDirError::new(format!(
                    "无法创建迁移目录 `{}`：{error}",
                    destination_path.display()
                ))
            })?;
            copy_legacy_tree_filtered(&source_path, &destination_path, legacy_root)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(DataDirError::new(format!(
                "legacy 数据包含不支持的文件类型 `{}`，已中止迁移。",
                source_path.display()
            )));
        }
        copy_file_without_overwrite(&source_path, &destination_path)?;
    }
    Ok(())
}

fn copy_file_without_overwrite(source: &Path, destination: &Path) -> Result<(), DataDirError> {
    let source_file = File::open(source).map_err(|error| {
        DataDirError::new(format!(
            "无法打开 legacy 文件 `{}`：{error}",
            source.display()
        ))
    })?;
    let destination_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)
        .map_err(|error| {
            DataDirError::new(format!(
                "无法创建迁移文件 `{}`：{error}",
                destination.display()
            ))
        })?;
    let mut reader = BufReader::new(source_file);
    let mut writer = BufWriter::new(destination_file);
    let copied = std::io::copy(&mut reader, &mut writer).map_err(|error| {
        DataDirError::new(format!(
            "复制 legacy 文件 `{}` 失败：{error}",
            source.display()
        ))
    })?;
    writer.flush().map_err(|error| {
        DataDirError::new(format!(
            "刷新迁移文件 `{}` 失败：{error}",
            destination.display()
        ))
    })?;
    writer.get_ref().sync_all().map_err(|error| {
        DataDirError::new(format!(
            "同步迁移文件 `{}` 失败：{error}",
            destination.display()
        ))
    })?;
    let expected = fs::metadata(source)
        .map_err(|error| {
            DataDirError::new(format!(
                "无法校验 legacy 文件 `{}`：{error}",
                source.display()
            ))
        })?
        .len();
    if copied != expected {
        return Err(DataDirError::new(format!(
            "legacy 文件 `{}` 复制不完整：预期 {expected} 字节，实际 {copied} 字节。",
            source.display()
        )));
    }
    Ok(())
}

fn apply_legacy_permissions(
    _source: &Path,
    destination: &Path,
    _root: bool,
) -> Result<(), DataDirError> {
    apply_private_tree_permissions(destination)
}

#[cfg(unix)]
fn apply_private_tree_permissions(path: &Path) -> Result<(), DataDirError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::symlink_metadata(path).map_err(|error| {
        DataDirError::new(format!("无法读取迁移权限 `{}`：{error}", path.display()))
    })?;
    let mode = if metadata.is_dir() { 0o700 } else { 0o600 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| {
        DataDirError::new(format!("无法收紧迁移权限 `{}`：{error}", path.display()))
    })?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|error| {
            DataDirError::new(format!("无法枚举迁移目录 `{}`：{error}", path.display()))
        })? {
            apply_private_tree_permissions(
                &entry
                    .map_err(|error| {
                        DataDirError::new(format!(
                            "无法读取迁移目录项 `{}`：{error}",
                            path.display()
                        ))
                    })?
                    .path(),
            )?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_private_tree_permissions(_path: &Path) -> Result<(), DataDirError> {
    Ok(())
}

fn make_marker_private(marker_path: &Path) -> Result<(), DataDirError> {
    #[cfg(not(unix))]
    let _ = marker_path;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(marker_path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            DataDirError::new(format!(
                "无法收紧迁移 marker `{}` 的访问权限：{error}",
                marker_path.display()
            ))
        })?;
    }
    Ok(())
}

fn write_json_file(path: &Path, value: &Value) -> Result<(), DataDirError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            DataDirError::new(format!(
                "无法创建迁移 JSON 目录 `{}`：{error}",
                parent.display()
            ))
        })?;
    }
    let mut content = serde_json::to_vec_pretty(value)
        .map_err(|error| DataDirError::new(format!("无法序列化迁移 JSON：{error}")))?;
    content.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|error| {
            DataDirError::new(format!("无法写入迁移 JSON `{}`：{error}", path.display()))
        })?;
    file.write_all(&content)
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            DataDirError::new(format!("无法同步迁移 JSON `{}`：{error}", path.display()))
        })
}

#[cfg(test)]
mod tests {
    use super::{AgentConfig, Config, LEGACY_MIGRATION_MARKER};
    use crate::domain::persona::{Persona, RoleplayStyle, ToolPolicy};
    use serde_json::{Value, json};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

    fn unique_temp_root(label: &str) -> PathBuf {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "muse-config-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("应能创建测试临时目录");
        root
    }

    fn write_json(path: &Path, value: &Value) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("应能创建测试 JSON 目录");
        }
        std::fs::write(
            path,
            serde_json::to_vec_pretty(value).expect("测试 JSON 应可序列化"),
        )
        .expect("应能写入测试 JSON");
    }

    #[test]
    fn data_dir_lock_does_not_create_target_and_rejects_a_second_holder() {
        let root = unique_temp_root("data-dir-lock");
        let target = root.join("not-created/muse-data");

        let lock = Config::acquire_data_dir_lock(&target).expect("首次应能取得数据目录锁");
        assert!(!target.exists(), "锁定迁移目标不应提前创建目标目录");
        assert!(lock.lock_path().is_file());

        let second =
            Config::acquire_data_dir_lock(&target).expect_err("同一路径不允许第二个写入者取得锁");
        assert!(second.to_string().contains("正被另一个 Muse 进程使用"));

        drop(lock);
        let retry = Config::acquire_data_dir_lock(&target).expect("释放后应可重新取得锁");
        drop(retry);
        std::fs::remove_dir_all(root).expect("应能清理测试目录");
    }

    fn sample_persona() -> Persona {
        Persona {
            id: "persona-a".to_string(),
            name: "测试角色".to_string(),
            summary: String::new(),
            character_profile: "冷静、可靠".to_string(),
            world_profile: String::new(),
            scenario: String::new(),
            system_prompt: "自由补充设定会限制回复很短。".to_string(),
            style: "简短直接".to_string(),
            roleplay_style: RoleplayStyle::LightNarration,
            dialogue_examples: String::new(),
            author_note: String::new(),
            opening_message: String::new(),
            tool_policy: ToolPolicy::default(),
            skill_policy: Default::default(),
            mcp_policy: Default::default(),
            default_visual_pack_id: "default-visual-pack".to_string(),
            author: String::new(),
            version: "1.0.0".to_string(),
            notes: String::new(),
        }
    }

    #[test]
    fn persona_prompt_keeps_roleplay_rules_as_output_shape() {
        let agent = AgentConfig {
            system_prompt: "你是{name}，角色特征是{personality}，说话风格是{speech_style}。"
                .to_string(),
            max_history: 50,
        };

        let prompt = agent.build_persona_system_prompt(&sample_persona());
        assert!(prompt.contains("【默认场景】"));
        assert!(prompt.contains("普通问候也要体现角色在场"));
        assert!(prompt.contains("即使用户只说“你好”"));

        let supplement_index = prompt.find("【角色补充设定】").expect("应包含角色补充设定");
        let roleplay_index = prompt.find("【角色演绎规则】").expect("应包含角色演绎规则");
        assert!(supplement_index < roleplay_index);
    }

    #[test]
    fn migration_copies_user_data_but_excludes_retired_model_files() {
        let root = unique_temp_root("legacy-copy");
        let legacy = root.join("workspace/.agent-vp-data");
        let target = root.join("home/.muse");
        let model_path = legacy.join("model-files/asr/whisper/ggml-small.bin");
        std::fs::create_dir_all(model_path.parent().expect("模型应有父目录"))
            .expect("应能创建旧模型目录");
        std::fs::write(&model_path, b"model-bytes").expect("应能写入旧模型文件");
        std::fs::create_dir_all(legacy.join("sessions/conversations")).expect("应能创建旧会话目录");
        std::fs::write(
            legacy.join("sessions/conversations/session-a.jsonl"),
            b"{\"kind\":\"user_message\"}\n",
        )
        .expect("应能写入旧会话");
        std::fs::create_dir_all(legacy.join("personas")).expect("应能创建旧角色目录");
        std::fs::write(legacy.join("personas/personas.json"), b"{\"personas\":[]}")
            .expect("应能写入旧角色文件");

        let assets = json!({
            "resources": [{
                "id": "asr-whisper-small",
                "kind": "asr",
                "engine": "whisper.cpp",
                "name": "Whisper Small",
                "status": "downloading",
                "version": "ggml-small",
                "path": "model-files/asr/whisper/ggml-small.bin",
                "source_url": "https://example.invalid/model.bin",
                "checksum": "a".repeat(64),
                "expected_size_bytes": 11,
                "size_bytes": 0,
                "notes": "测试模型",
                "last_error": "旧错误",
                "last_error_kind": "network"
            }]
        });
        let config = json!({
            "speech_recognition": {
                "profiles": [
                    {"id": "valid", "model_path": "model-files/asr/whisper/ggml-small.bin"},
                    {"id": "missing", "model_path": "model-files/asr/whisper/missing.bin"}
                ]
            }
        });
        write_json(&legacy.join("models/assets.json"), &assets);
        write_json(&legacy.join("models/config.json"), &config);
        let original_assets =
            std::fs::read(legacy.join("models/assets.json")).expect("应能读取原资产清单");
        let original_config =
            std::fs::read(legacy.join("models/config.json")).expect("应能读取原模型配置");
        std::fs::create_dir_all(&target).expect("应能预先创建空目标目录");

        let report = Config::migrate_legacy_workspace_data(&target, &legacy)
            .expect("legacy 迁移应成功")
            .expect("空目标应执行迁移");

        assert_eq!(report.external_model_assets, 0);
        assert_eq!(report.external_model_paths, 0);
        assert!(report.excluded_model_files);
        assert!(target.join("personas/personas.json").is_file());
        assert!(
            target
                .join("sessions/conversations/session-a.jsonl")
                .is_file()
        );
        assert!(!target.join("model-files").exists());
        assert!(!target.join("models/assets.json").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&target)
                    .expect("应读取 Muse 数据目录权限")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        assert!(target.join(LEGACY_MIGRATION_MARKER).is_file());

        let migrated_config: Value = serde_json::from_slice(
            &std::fs::read(target.join("models/config.json")).expect("应能读取迁移后的模型配置"),
        )
        .expect("迁移后的模型配置应为 JSON");
        assert_eq!(
            migrated_config["speech_recognition"]["profiles"][0]["model_path"],
            "model-files/asr/whisper/ggml-small.bin"
        );
        assert_eq!(
            migrated_config["speech_recognition"]["profiles"][1]["model_path"],
            "model-files/asr/whisper/missing.bin"
        );
        assert_eq!(
            std::fs::read(legacy.join("models/assets.json")).expect("原资产清单应保留"),
            original_assets
        );
        assert_eq!(
            std::fs::read(legacy.join("models/config.json")).expect("原模型配置应保留"),
            original_config
        );

        std::fs::remove_dir_all(root).expect("应能清理测试临时目录");
    }

    #[test]
    fn desktop_migration_never_overwrites_non_empty_target() {
        let root = unique_temp_root("legacy-no-overwrite");
        let legacy = root.join("workspace/.agent-vp-data");
        let target = root.join("home/.muse");
        std::fs::create_dir_all(&legacy).expect("应能创建 legacy 目录");
        std::fs::write(legacy.join("legacy.txt"), b"legacy").expect("应能写入 legacy 文件");
        std::fs::create_dir_all(&target).expect("应能创建目标目录");
        std::fs::write(target.join("keep.txt"), b"keep").expect("应能写入目标文件");

        let report = Config::migrate_legacy_workspace_data(&target, &legacy)
            .expect("非空目标应安全跳过迁移");

        assert!(report.is_none());
        assert_eq!(
            std::fs::read(target.join("keep.txt")).expect("目标文件应保留"),
            b"keep"
        );
        assert!(!target.join("legacy.txt").exists());
        std::fs::remove_dir_all(root).expect("应能清理测试临时目录");
    }

    #[test]
    fn desktop_migration_ignores_retired_model_asset_manifest() {
        let root = unique_temp_root("legacy-retry");
        let legacy = root.join("workspace/.agent-vp-data");
        let target = root.join("home/.muse");
        std::fs::create_dir_all(legacy.join("models")).expect("应能创建 legacy 模型目录");
        std::fs::write(legacy.join("models/assets.json"), b"{").expect("应能写入损坏的旧资产清单");

        let report = Config::migrate_legacy_workspace_data(&target, &legacy)
            .expect("退场资产清单不得阻塞迁移")
            .expect("空目标应实际完成迁移");
        assert_eq!(report.external_model_assets, 0);
        assert!(report.excluded_model_files);
        assert!(!target.join("models/assets.json").exists());
        assert!(target.join(LEGACY_MIGRATION_MARKER).is_file());

        std::fs::remove_dir_all(root).expect("应能清理测试临时目录");
    }
}
