//! v3 会话事件存储、单写者和旧 transcript 迁移。
//!
//! 新格式使用 generation 目录承载不可变迁移结果，以 `store.json` 原子指针
//! 选择当前 generation。运行期追加由进程内唯一互斥写者串行化，避免多任务
//! 同时追加同一 JSONL 文件造成行交错。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;

pub const SESSION_EVENT_SCHEMA_VERSION: &str = "muse-session-event/v3";
pub const SESSION_STORE_SCHEMA_VERSION: &str = "muse-session-store/v3";
const MIGRATOR_VERSION: &str = "legacy-jsonl-to-v3/1";
const DEFAULT_CONVERSATION_ID: &str = "default";
const RUNTIME_MANIFEST_SCHEMA_VERSION: &str = "muse-session-runtime-manifest/v1";

/// v3 会话事件。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionEventV3 {
    pub schema_version: String,
    pub event_id: String,
    pub commit_seq: u64,
    pub conversation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub kind: String,
    pub time: String,
    pub payload: Value,
    /// 仅终态事件携带，避免恢复逻辑依赖自由文本推断回合结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_outcome: Option<String>,
    /// 迁移事件保留完整旧记录，避免未知顶层字段只存在于备份里。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_record: Option<Value>,
    /// 迁移来源用于跨启动按来源顺序对齐镜像记录，不参与普通运行时事件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_source: Option<LegacyEventSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyEventSource {
    pub relative_path: String,
    pub source_kind: String,
    pub byte_offset: usize,
}

/// 会话存储打开或迁移结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMigrationReport {
    pub migration_id: String,
    pub generation_id: String,
    pub legacy_sources: usize,
    pub source_records: usize,
    pub migrated_records: usize,
    pub deduplicated_records: usize,
    pub quarantined_records: usize,
    pub reused_existing_store: bool,
    pub backup_dir: Option<PathBuf>,
}

/// SQLite 会话索引用来判断自身是否仍与当前 JSONL generation 一致的稳定身份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIndexIdentity {
    pub generation_id: String,
    pub manifest_hash: String,
}

/// 会话存储错误。
#[derive(Debug)]
pub enum SessionStoreError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    InvalidJson {
        path: PathBuf,
        source: serde_json::Error,
    },
    InvalidStore(String),
    InvalidInput(String),
}

impl fmt::Display for SessionStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "{operation}会话存储 `{}` 失败：{source}",
                path.display()
            ),
            Self::InvalidJson { path, source } => write!(
                formatter,
                "会话存储 `{}` 不是有效 JSON：{source}",
                path.display()
            ),
            Self::InvalidStore(message) | Self::InvalidInput(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for SessionStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidJson { source, .. } => Some(source),
            Self::InvalidStore(_) | Self::InvalidInput(_) => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StorePointer {
    schema_version: String,
    active_generation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_generation: Option<String>,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GenerationManifest {
    schema_version: String,
    generation_id: String,
    migration_id: String,
    created_at: String,
    legacy_sources: usize,
    source_records: usize,
    migrated_records: usize,
    deduplicated_records: usize,
    quarantined_records: usize,
    last_commit_seq: u64,
    conversations: BTreeMap<String, String>,
    #[serde(default)]
    legacy_semantic_counts: BTreeMap<String, LegacySemanticCounts>,
}

/// 活动 generation 的可变高水位清单。
///
/// `manifest.json` 只描述迁移发布时的不可变输入；运行期追加、序号保留和会话
/// 删除写入本清单，避免把迁移事实与活动写入状态混在同一份文件中。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RuntimeManifest {
    schema_version: String,
    generation_id: String,
    updated_at: String,
    /// 已保留的最大序号，不要求磁盘事件连续。写入失败也不能回退该高水位。
    last_commit_seq: u64,
    conversations: BTreeMap<String, String>,
    /// 新会话索引已经保留，但首条事件可能尚未完成。启动恢复只允许移除这些
    /// 明确标记的空悬索引，不能把普通缺失文件误判为安全失败。
    #[serde(default)]
    pending_conversations: BTreeSet<String>,
    /// 删除先记账再落盘；崩溃恢复据此幂等完成删除。
    #[serde(default)]
    pending_deletions: BTreeMap<String, String>,
    #[serde(default)]
    removed_legacy_records: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LegacySemanticCounts {
    global: usize,
    conversation: usize,
}

#[derive(Debug)]
struct WriterState {
    next_commit_seq: u64,
    runtime_manifest: RuntimeManifest,
    poison_reason: Option<String>,
}

impl RuntimeManifest {
    fn from_generation_manifest(manifest: &GenerationManifest) -> Self {
        Self {
            schema_version: RUNTIME_MANIFEST_SCHEMA_VERSION.to_string(),
            generation_id: manifest.generation_id.clone(),
            updated_at: Utc::now().to_rfc3339(),
            last_commit_seq: manifest.last_commit_seq,
            conversations: manifest.conversations.clone(),
            pending_conversations: BTreeSet::new(),
            pending_deletions: BTreeMap::new(),
            removed_legacy_records: 0,
        }
    }
}

#[derive(Debug)]
struct SessionStoreInner {
    sessions_dir: PathBuf,
    generation_id: String,
    generation_dir: PathBuf,
    writer: Arc<Mutex<WriterState>>,
}

static WRITER_REGISTRY: OnceLock<StdMutex<HashMap<PathBuf, Weak<Mutex<WriterState>>>>> =
    OnceLock::new();
static STORE_OPEN_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static BACKUP_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// v3 会话存储句柄。
#[derive(Debug, Clone)]
pub struct SessionStore {
    inner: Arc<SessionStoreInner>,
}

impl SessionStore {
    /// 以应用数据根目录打开存储；实际路径为 `<data_dir>/sessions`。
    pub async fn open(
        data_dir: impl AsRef<Path>,
    ) -> Result<(Self, SessionMigrationReport), SessionStoreError> {
        Self::open_sessions_dir(data_dir.as_ref().join("sessions")).await
    }

    /// 以明确的 `sessions` 目录打开存储，便于现有 Web 层渐进接入。
    pub async fn open_sessions_dir(
        sessions_dir: impl AsRef<Path>,
    ) -> Result<(Self, SessionMigrationReport), SessionStoreError> {
        let sessions_dir = sessions_dir.as_ref().to_path_buf();
        create_dir_all(&sessions_dir).await?;
        let sessions_metadata = fs::symlink_metadata(&sessions_dir)
            .await
            .map_err(|source| io_error("检查", &sessions_dir, source))?;
        if metadata_is_link_or_reparse(&sessions_metadata) || !sessions_metadata.is_dir() {
            return Err(SessionStoreError::InvalidStore(format!(
                "会话存储根目录 `{}` 不是普通目录或包含符号链接。",
                sessions_dir.display()
            )));
        }
        set_private_directory_permissions(&sessions_dir)?;
        let sessions_dir = fs::canonicalize(&sessions_dir)
            .await
            .map_err(|source| io_error("规范化", &sessions_dir, source))?;
        // 打开和首次迁移发生频率很低，全局串行可避免两个调用同时发布 generation。
        let _open_guard = STORE_OPEN_LOCK.get_or_init(|| Mutex::new(())).lock().await;

        let pointer_path = sessions_dir.join("store.json");
        if path_exists(&pointer_path).await? {
            let pointer: StorePointer = read_json(&pointer_path).await?;
            validate_pointer(&pointer)?;
            let generation_dir = sessions_dir
                .join("generations")
                .join(&pointer.active_generation);
            validate_owned_directory(&sessions_dir, &generation_dir).await?;
            validate_owned_directory(&sessions_dir, &generation_dir.join("conversations")).await?;
            let manifest = load_manifest(&generation_dir).await?;
            if manifest.generation_id != pointer.active_generation {
                return Err(SessionStoreError::InvalidStore(format!(
                    "存储指针指向 `{}`，但 generation 清单标识为 `{}`。",
                    pointer.active_generation, manifest.generation_id
                )));
            }
            repair_runtime_event_tails(&sessions_dir, &generation_dir).await?;
            let runtime_manifest =
                load_or_rebuild_runtime_manifest(&generation_dir, &manifest).await?;
            validate_generation_for_publication(&generation_dir, &pointer.active_generation)
                .await?;
            if let Some(migration) = migrate_appended_legacy_sources(
                &sessions_dir,
                &pointer,
                &manifest,
                &runtime_manifest,
                &generation_dir,
            )
            .await?
            {
                let store = Self::from_generation(
                    sessions_dir,
                    migration.report.generation_id.clone(),
                    migration.generation_dir,
                    migration.runtime_manifest,
                );
                return Ok((store, migration.report));
            }
            let writer = shared_writer(&generation_dir, runtime_manifest);
            let backup_dir = (manifest.legacy_sources > 0)
                .then(|| sessions_dir.join("backups").join(&manifest.migration_id));
            let store = Self::from_generation_with_writer(
                sessions_dir,
                pointer.active_generation.clone(),
                generation_dir,
                writer,
            );
            return Ok((
                store,
                SessionMigrationReport {
                    migration_id: manifest.migration_id,
                    generation_id: pointer.active_generation,
                    legacy_sources: manifest.legacy_sources,
                    source_records: manifest.source_records,
                    migrated_records: manifest.migrated_records,
                    deduplicated_records: manifest.deduplicated_records,
                    quarantined_records: manifest.quarantined_records,
                    reused_existing_store: true,
                    backup_dir,
                },
            ));
        }

        let migration = migrate_legacy_sources(&sessions_dir).await?;
        let store = Self::from_generation(
            sessions_dir,
            migration.report.generation_id.clone(),
            migration.generation_dir,
            migration.runtime_manifest,
        );
        Ok((store, migration.report))
    }

    fn from_generation(
        sessions_dir: PathBuf,
        generation_id: String,
        generation_dir: PathBuf,
        runtime_manifest: RuntimeManifest,
    ) -> Self {
        let writer = shared_writer(&generation_dir, runtime_manifest);
        Self::from_generation_with_writer(sessions_dir, generation_id, generation_dir, writer)
    }

    fn from_generation_with_writer(
        sessions_dir: PathBuf,
        generation_id: String,
        generation_dir: PathBuf,
        writer: Arc<Mutex<WriterState>>,
    ) -> Self {
        Self {
            inner: Arc::new(SessionStoreInner {
                sessions_dir,
                generation_id,
                generation_dir,
                writer,
            }),
        }
    }

    pub fn sessions_dir(&self) -> &Path {
        &self.inner.sessions_dir
    }

    pub fn generation_id(&self) -> &str {
        &self.inner.generation_id
    }

    /// 返回不包含绝对路径和会话正文的运行清单摘要。
    pub async fn index_identity(&self) -> Result<SessionIndexIdentity, SessionStoreError> {
        let writer = self.inner.writer.lock().await;
        self.validate_active_storage_paths().await?;
        let material = serde_json::to_vec(&serde_json::json!({
            "generation_id": self.inner.generation_id,
            "last_commit_seq": writer.runtime_manifest.last_commit_seq,
            "conversations": writer.runtime_manifest.conversations,
            "pending_conversations": writer.runtime_manifest.pending_conversations,
            "pending_deletions": writer.runtime_manifest.pending_deletions,
        }))
        .map_err(|source| SessionStoreError::InvalidJson {
            path: self.inner.generation_dir.clone(),
            source,
        })?;
        Ok(SessionIndexIdentity {
            generation_id: self.inner.generation_id.clone(),
            manifest_hash: format!("{:x}", Sha256::digest(material)),
        })
    }

    /// 通过唯一进程内写者追加事件。
    pub async fn append_event(
        &self,
        conversation_id: impl Into<String>,
        turn_id: Option<String>,
        kind: impl Into<String>,
        payload: Value,
    ) -> Result<SessionEventV3, SessionStoreError> {
        let conversation_id = conversation_id.into();
        let kind = kind.into();
        validate_non_empty("conversation_id", &conversation_id)?;
        validate_non_empty("kind", &kind)?;

        let mut writer = self.inner.writer.lock().await;
        if let Some(reason) = &writer.poison_reason {
            return Err(SessionStoreError::InvalidStore(format!(
                "会话写者已进入保护状态，必须重启并恢复活动 generation 后才能继续写入：{reason}"
            )));
        }
        self.validate_active_storage_paths().await?;
        let path = self.conversation_path(&conversation_id);
        let is_new_conversation = !path_exists(&path).await?;
        let completes_pending_conversation = is_new_conversation
            || writer
                .runtime_manifest
                .pending_conversations
                .contains(&conversation_id);
        let commit_seq = writer.next_commit_seq;
        let next_commit_seq = commit_seq
            .checked_add(1)
            .ok_or_else(|| SessionStoreError::InvalidStore("会话事件序号已经耗尽。".to_string()))?;
        // 先消耗内存序号并持久化高水位，再接触 transcript 文件。即使后续写入
        // 安全回滚或进程崩溃，该序号也不会在本进程或重启后被复用。
        writer.next_commit_seq = next_commit_seq;
        let file_name = conversation_file_name(&conversation_id);
        let mut reserved_manifest = writer.runtime_manifest.clone();
        reserved_manifest.last_commit_seq = commit_seq;
        reserved_manifest
            .conversations
            .insert(conversation_id.clone(), file_name);
        if is_new_conversation {
            reserved_manifest
                .pending_conversations
                .insert(conversation_id.clone());
        }
        reserved_manifest.updated_at = Utc::now().to_rfc3339();
        if let Err(error) =
            persist_runtime_manifest(&self.inner.generation_dir, &reserved_manifest).await
        {
            writer.poison_reason = Some(error.to_string());
            return Err(error);
        }
        writer.runtime_manifest = reserved_manifest;

        let time = Utc::now().to_rfc3339();
        let event_id = hash_text(&format!(
            "{}\0{commit_seq}\0{time}\0{conversation_id}\0{kind}\0{}",
            self.inner.generation_id,
            serde_json::to_string(&payload).map_err(|source| SessionStoreError::InvalidJson {
                path: self.inner.generation_dir.clone(),
                source,
            })?
        ));
        let event = SessionEventV3 {
            schema_version: SESSION_EVENT_SCHEMA_VERSION.to_string(),
            event_id,
            commit_seq,
            conversation_id: conversation_id.clone(),
            turn_id,
            turn_outcome: terminal_outcome_for_kind(&kind),
            kind,
            time,
            payload,
            legacy_record: None,
            legacy_source: None,
        };
        let mut line =
            serde_json::to_vec(&event).map_err(|source| SessionStoreError::InvalidJson {
                path: path.clone(),
                source,
            })?;
        line.push(b'\n');
        if let Err(failure) = append_bytes_with_recovery(&path, &line).await {
            if failure.poison_writer {
                writer.poison_reason = Some(failure.error.to_string());
            }
            return Err(failure.error);
        }
        if completes_pending_conversation {
            let mut committed_manifest = writer.runtime_manifest.clone();
            committed_manifest
                .pending_conversations
                .remove(&conversation_id);
            committed_manifest.updated_at = Utc::now().to_rfc3339();
            if let Err(error) =
                persist_runtime_manifest(&self.inner.generation_dir, &committed_manifest).await
            {
                writer.poison_reason = Some(error.to_string());
                return Err(error);
            }
            writer.runtime_manifest = committed_manifest;
        }
        Ok(event)
    }

    /// 读取指定会话的事件，文件名仅由会话 ID 哈希生成。
    pub async fn events_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<SessionEventV3>, SessionStoreError> {
        validate_non_empty("conversation_id", conversation_id)?;
        let writer = self.inner.writer.lock().await;
        self.validate_active_storage_paths().await?;
        if writer.runtime_manifest.conversations.get(conversation_id)
            != Some(&conversation_file_name(conversation_id))
        {
            return Ok(Vec::new());
        }
        let path = self.conversation_path(conversation_id);
        if !path_exists(&path).await? {
            return Ok(Vec::new());
        }
        read_event_file(&path).await
    }

    /// 虚拟聚合当前 generation 中的全部事件，不再维护第二份全局 JSONL。
    pub async fn aggregate_events(&self) -> Result<Vec<SessionEventV3>, SessionStoreError> {
        let writer = self.inner.writer.lock().await;
        self.validate_active_storage_paths().await?;
        let conversations_dir = self.inner.generation_dir.join("conversations");
        let actual_files = conversation_file_names(&conversations_dir).await?;
        let expected_files = writer
            .runtime_manifest
            .conversations
            .values()
            .cloned()
            .collect::<BTreeSet<_>>();
        if actual_files != expected_files {
            return Err(SessionStoreError::InvalidStore(format!(
                "活动 generation 会话文件与运行清单不一致：清单 {} 个，磁盘 {} 个。",
                expected_files.len(),
                actual_files.len()
            )));
        }
        let mut events = Vec::new();
        for file_name in actual_files {
            events.extend(read_event_file(&conversations_dir.join(file_name)).await?);
        }
        events.sort_by_key(|event| event.commit_seq);
        Ok(events)
    }

    /// 删除指定会话的 canonical 事件文件。
    ///
    /// 调用方必须先持有运行时空闲维护租约；writer 锁保证删除与追加不会交错。
    pub async fn delete_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<usize, SessionStoreError> {
        self.delete_conversation_with_completion(conversation_id, |path| async move {
            fs::remove_file(&path)
                .await
                .map_err(|source| io_error("删除", &path, source))?;
            if let Some(parent) = path.parent() {
                sync_directory(parent).await?;
            }
            Ok(())
        })
        .await
    }

    async fn delete_conversation_with_completion<Complete, CompleteFuture>(
        &self,
        conversation_id: &str,
        complete: Complete,
    ) -> Result<usize, SessionStoreError>
    where
        Complete: FnOnce(PathBuf) -> CompleteFuture,
        CompleteFuture: Future<Output = Result<(), SessionStoreError>>,
    {
        validate_non_empty("conversation_id", conversation_id)?;
        let mut writer = self.inner.writer.lock().await;
        if let Some(reason) = &writer.poison_reason {
            return Err(SessionStoreError::InvalidStore(format!(
                "会话写者已进入保护状态，必须重启并恢复活动 generation 后才能删除：{reason}"
            )));
        }
        self.validate_active_storage_paths().await?;
        if writer.runtime_manifest.conversations.get(conversation_id)
            != Some(&conversation_file_name(conversation_id))
        {
            return Ok(0);
        }
        let path = self.conversation_path(conversation_id);
        if !path_exists(&path).await? {
            return Ok(0);
        }
        let events = read_event_file(&path).await?;
        let records = events.len();
        let removed_legacy_records = events
            .iter()
            .filter(|event| event.legacy_record.is_some())
            .count();
        let mut runtime_manifest = writer.runtime_manifest.clone();
        runtime_manifest.conversations.remove(conversation_id);
        runtime_manifest.pending_deletions.insert(
            conversation_id.to_string(),
            conversation_file_name(conversation_id),
        );
        runtime_manifest.removed_legacy_records = runtime_manifest
            .removed_legacy_records
            .checked_add(removed_legacy_records)
            .ok_or_else(|| {
                SessionStoreError::InvalidStore("已删除迁移事件计数溢出。".to_string())
            })?;
        runtime_manifest.updated_at = Utc::now().to_rfc3339();
        if let Err(error) =
            persist_runtime_manifest(&self.inner.generation_dir, &runtime_manifest).await
        {
            writer.poison_reason = Some(error.to_string());
            return Err(error);
        }
        writer.runtime_manifest = runtime_manifest;
        if let Err(error) = complete(path).await {
            // pending deletion 已经成为磁盘事实；此后任何失败都不能让本进程
            // 继续基于“已删除”或“未删除”的猜测追加事件。重启会按 pending
            // 清单幂等完成删除。
            writer.poison_reason = Some(error.to_string());
            return Err(error);
        }
        let mut completed_manifest = writer.runtime_manifest.clone();
        completed_manifest.pending_deletions.remove(conversation_id);
        completed_manifest.updated_at = Utc::now().to_rfc3339();
        if let Err(error) =
            persist_runtime_manifest(&self.inner.generation_dir, &completed_manifest).await
        {
            writer.poison_reason = Some(error.to_string());
            return Err(error);
        }
        writer.runtime_manifest = completed_manifest;
        Ok(records)
    }

    fn conversation_path(&self, conversation_id: &str) -> PathBuf {
        self.inner
            .generation_dir
            .join("conversations")
            .join(conversation_file_name(conversation_id))
    }

    async fn validate_active_storage_paths(&self) -> Result<(), SessionStoreError> {
        // 应用数据目录独占锁保证协作中的 Muse 进程不会替换父目录；这里仍在每次
        // 操作前逐级核对真实 generation/conversations 身份，而最终文件必须再以
        // O_NOFOLLOW（Windows 为 OPEN_REPARSE_POINT）打开，不能把目录校验当成
        // 最终文件的 TOCTOU 防护。
        validate_owned_directory(&self.inner.sessions_dir, &self.inner.generation_dir).await?;
        validate_owned_directory(
            &self.inner.sessions_dir,
            &self.inner.generation_dir.join("conversations"),
        )
        .await
    }
}

/// 将不可信会话 ID 映射为固定长度文件名，阻断路径穿越和名称碰撞覆盖。
pub fn conversation_file_name(conversation_id: &str) -> String {
    format!("{}.jsonl", hash_text(conversation_id))
}

#[derive(Debug)]
struct MigrationOutput {
    report: SessionMigrationReport,
    generation_dir: PathBuf,
    runtime_manifest: RuntimeManifest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacySourceKind {
    Global,
    Conversation,
}

#[derive(Debug)]
struct LegacySource {
    path: PathBuf,
    relative_path: PathBuf,
    kind: LegacySourceKind,
    bytes: Vec<u8>,
    base_byte_offset: usize,
}

#[derive(Debug, Clone)]
struct LegacyRecord {
    source_relative_path: PathBuf,
    source_kind: LegacySourceKind,
    source_order: usize,
    byte_offset: usize,
    value: Value,
    conversation_id: String,
    turn_id: Option<String>,
    kind: String,
    time: Option<String>,
    payload: Value,
    semantic_hash: String,
    raw_hash: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct QuarantineRecord {
    source_path: String,
    byte_offset: usize,
    raw_hex: String,
    error: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RuntimeQuarantineRecord {
    source_path: String,
    byte_offset: u64,
    raw_hex: String,
    error: String,
    quarantined_at: String,
}

#[derive(Debug)]
struct AppendFailure {
    error: SessionStoreError,
    poison_writer: bool,
}

#[derive(Debug, Clone, Copy)]
enum SecureFileMode {
    Read,
    ReadWrite,
    AppendOrCreate,
    CreateNew,
}

/// 以不跟随最终路径符号链接/重解析点的方式打开普通文件。
///
/// 所有校验和读写均继续使用返回的同一文件句柄，避免先检查路径 A、随后却因
/// 路径替换而读写文件 B 的 TOCTOU 窗口。
fn open_regular_file_no_follow(
    path: &Path,
    mode: SecureFileMode,
    operation: &'static str,
) -> Result<fs::File, SessionStoreError> {
    let mut options = std::fs::OpenOptions::new();
    match mode {
        SecureFileMode::Read => {
            options.read(true);
        }
        SecureFileMode::ReadWrite => {
            options.read(true).write(true);
        }
        SecureFileMode::AppendOrCreate => {
            options.read(true).append(true).create(true);
        }
        SecureFileMode::CreateNew => {
            options.read(true).write(true).create_new(true);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(path)
        .map_err(|source| io_error(operation, path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| io_error("检查已打开文件", path, source))?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(SessionStoreError::InvalidStore(format!(
                "拒绝打开重解析点 `{}`。",
                path.display()
            )));
        }
    }
    if !metadata.is_file() {
        return Err(SessionStoreError::InvalidStore(format!(
            "会话存储目标 `{}` 不是普通文件。",
            path.display()
        )));
    }
    #[cfg(unix)]
    if !matches!(mode, SecureFileMode::Read) {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|source| io_error("收紧文件权限", path, source))?;
    }
    Ok(fs::File::from_std(file))
}

fn metadata_is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}

async fn read_open_file_from_start(file: &mut fs::File) -> Result<Vec<u8>, std::io::Error> {
    file.seek(std::io::SeekFrom::Start(0)).await?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    Ok(bytes)
}

async fn read_regular_file_no_follow(path: &Path) -> Result<Vec<u8>, SessionStoreError> {
    let mut file = open_regular_file_no_follow(path, SecureFileMode::Read, "安全打开")?;
    read_open_file_from_start(&mut file)
        .await
        .map_err(|source| io_error("读取", path, source))
}

/// 已发布 v3 generation 后若旧版程序继续向 legacy 文件追加记录，下一次启动会
/// 基于上一份逐字节备份只导入新增后缀，并把当前 canonical 事件完整带入新 generation。
async fn migrate_appended_legacy_sources(
    sessions_dir: &Path,
    pointer: &StorePointer,
    previous_manifest: &GenerationManifest,
    previous_runtime_manifest: &RuntimeManifest,
    previous_generation_dir: &Path,
) -> Result<Option<MigrationOutput>, SessionStoreError> {
    let sources = discover_legacy_sources(sessions_dir).await?;
    let current_migration_id = migration_id(&sources);
    let previous_backup_dir = sessions_dir
        .join("backups")
        .join(&previous_manifest.migration_id);
    if current_migration_id == previous_manifest.migration_id {
        // 即使无需迁移，也逐字节复验当前源对应的只读备份。缺失备份可从
        // 未变化的 legacy 源安全重建；已有但内容不同的备份必须拒绝覆盖。
        backup_sources(&sources, sessions_dir, &previous_backup_dir).await?;
        return Ok(None);
    }

    let previous_sources = if previous_manifest.legacy_sources == 0 {
        Vec::new()
    } else {
        validate_owned_directory(sessions_dir, &previous_backup_dir).await?;
        discover_legacy_sources(&previous_backup_dir).await?
    };
    if previous_sources.len() != previous_manifest.legacy_sources {
        return Err(SessionStoreError::InvalidStore(format!(
            "上一 generation 的 legacy 备份不完整：清单 {} 份，实际 {} 份。",
            previous_manifest.legacy_sources,
            previous_sources.len()
        )));
    }
    if migration_id(&previous_sources) != previous_manifest.migration_id {
        return Err(SessionStoreError::InvalidStore(
            "上一 generation 的 legacy 备份长度或 SHA-256 与迁移清单不一致。".to_string(),
        ));
    }
    for previous_source in &previous_sources {
        if !sources
            .iter()
            .any(|source| source.relative_path == previous_source.relative_path)
        {
            return Err(SessionStoreError::InvalidStore(format!(
                "旧会话源 `{}` 在上次迁移后消失，已拒绝从 canonical 会话中删除历史。",
                previous_source.relative_path.display()
            )));
        }
    }
    for source in &sources {
        let previous_path = previous_backup_dir.join(&source.relative_path);
        let previous_bytes = if path_exists(&previous_path).await? {
            read_regular_file_no_follow(&previous_path).await?
        } else {
            Vec::new()
        };
        if !source.bytes.starts_with(&previous_bytes) {
            return Err(SessionStoreError::InvalidStore(format!(
                "旧会话源 `{}` 在上次迁移后被改写而非追加，已拒绝覆盖 canonical 会话。",
                source.path.display()
            )));
        }
    }

    let generation_id = format!(
        "gen-{}",
        hash_text(&format!(
            "{}\0{}\0{}",
            pointer.active_generation,
            current_migration_id,
            previous_runtime_manifest.last_commit_seq
        ))
    );
    let generation_dir = sessions_dir.join("generations").join(&generation_id);
    let backup_dir = sessions_dir.join("backups").join(&current_migration_id);
    backup_sources(&sources, sessions_dir, &backup_dir).await?;
    if path_exists(&generation_dir).await? {
        let manifest = load_manifest(&generation_dir).await?;
        if manifest.migration_id != current_migration_id {
            return Err(SessionStoreError::InvalidStore(format!(
                "增量 generation `{generation_id}` 的迁移标识不匹配。"
            )));
        }
        validate_generation_for_publication(&generation_dir, &generation_id).await?;
        activate_generation(
            sessions_dir,
            &generation_id,
            Some(pointer.active_generation.clone()),
        )
        .await?;
        let runtime_manifest = load_runtime_manifest(&generation_dir).await?;
        return Ok(Some(MigrationOutput {
            report: SessionMigrationReport {
                migration_id: current_migration_id,
                generation_id,
                legacy_sources: manifest.legacy_sources,
                source_records: manifest.source_records,
                migrated_records: manifest.migrated_records,
                deduplicated_records: manifest.deduplicated_records,
                quarantined_records: manifest.quarantined_records,
                reused_existing_store: true,
                backup_dir: (!sources.is_empty()).then_some(backup_dir),
            },
            generation_dir,
            runtime_manifest,
        }));
    }

    // 每次 legacy 源发生追加时都从完整只读备份输入重建 canonical legacy
    // 序列。只对 delta 做 LCS 再尾追加无法把迟到的镜像锚点插回正确位置，
    // 例如首轮 A,C / A，次轮分会话追加 B,C 会错误得到 A,C,B。
    let mut records = Vec::new();
    let mut quarantined = Vec::new();
    let mut source_records = 0usize;
    for (source_order, source) in sources.iter().enumerate() {
        parse_source(
            source,
            source_order,
            &mut records,
            &mut quarantined,
            &mut source_records,
        );
    }
    let mut legacy_semantic_counts = BTreeMap::new();
    add_semantic_counts(&mut legacy_semantic_counts, &records);
    let (records, deduplicated_records) = merge_legacy_mirrors(records);

    let mut previous_events = Vec::<SessionEventV3>::new();
    let previous_conversations_dir = previous_generation_dir.join("conversations");
    if path_exists(&previous_conversations_dir).await? {
        for path in read_file_entries(&previous_conversations_dir).await? {
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                continue;
            }
            for event in read_event_file(&path).await? {
                previous_events.push(event);
            }
        }
    }
    previous_events.sort_by_key(|event| event.commit_seq);

    let mut rebuilt_legacy_events = Vec::<SessionEventV3>::with_capacity(records.len());
    for record in records {
        let event_id = hash_text(&format!(
            "{}\0{}\0{}\0{}",
            current_migration_id,
            record.source_relative_path.display(),
            record.byte_offset,
            record.raw_hash
        ));
        let turn_outcome = terminal_outcome_for_kind(&record.kind);
        let legacy_source = legacy_event_source(&record);
        rebuilt_legacy_events.push(SessionEventV3 {
            schema_version: SESSION_EVENT_SCHEMA_VERSION.to_string(),
            event_id,
            commit_seq: 0,
            conversation_id: record.conversation_id,
            turn_id: record.turn_id,
            kind: record.kind,
            time: record
                .time
                .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string()),
            payload: record.payload,
            turn_outcome,
            legacy_record: Some(record.value),
            legacy_source: Some(legacy_source),
        });
    }

    let previous_legacy_hashes = previous_events
        .iter()
        .filter_map(|event| event.legacy_record.as_ref())
        .map(legacy_value_semantic_hash)
        .collect::<Vec<_>>();
    let rebuilt_legacy_hashes = rebuilt_legacy_events
        .iter()
        .filter_map(|event| event.legacy_record.as_ref())
        .map(legacy_value_semantic_hash)
        .collect::<Vec<_>>();
    let legacy_anchors = semantic_lcs_matches(&previous_legacy_hashes, &rebuilt_legacy_hashes);
    if legacy_anchors.len() != previous_legacy_hashes.len() {
        return Err(SessionStoreError::InvalidStore(
            "完整 legacy 重建无法对齐上一 generation 的全部 canonical 事件，已拒绝改写历史。"
                .to_string(),
        ));
    }
    let anchor_by_previous = legacy_anchors.into_iter().collect::<BTreeMap<_, _>>();

    // 以旧 generation 的完整 commit_seq 时间线为骨架：有后续 legacy 锚点的
    // 迟到镜像在该锚点前插入；没有后续锚点的 trailing unique 在旧时间线
    // （包括其后的 native v3）之后追加，保留真实观察顺序。
    let native_count = previous_events
        .iter()
        .filter(|event| event.legacy_record.is_none())
        .count();
    let mut ordered_events = Vec::<SessionEventV3>::with_capacity(
        rebuilt_legacy_events.len().saturating_add(native_count),
    );
    let mut previous_legacy_index = 0usize;
    let mut rebuilt_legacy_cursor = 0usize;
    for previous_event in previous_events {
        if previous_event.legacy_record.is_some() {
            let rebuilt_anchor = anchor_by_previous
                .get(&previous_legacy_index)
                .copied()
                .ok_or_else(|| {
                    SessionStoreError::InvalidStore(
                        "上一 generation 的 legacy 锚点缺失。".to_string(),
                    )
                })?;
            if rebuilt_anchor < rebuilt_legacy_cursor {
                return Err(SessionStoreError::InvalidStore(
                    "legacy 锚点顺序发生回退。".to_string(),
                ));
            }
            ordered_events.extend(
                rebuilt_legacy_events[rebuilt_legacy_cursor..=rebuilt_anchor]
                    .iter()
                    .cloned(),
            );
            rebuilt_legacy_cursor = rebuilt_anchor.saturating_add(1);
            previous_legacy_index = previous_legacy_index.saturating_add(1);
        } else {
            ordered_events.push(previous_event);
        }
    }
    ordered_events.extend(
        rebuilt_legacy_events[rebuilt_legacy_cursor..]
            .iter()
            .cloned(),
    );
    // 新 generation 会为完整时间线重新编号，以便把迟到的 legacy 镜像插回
    // 正确位置。编号必须整体越过上一 generation 已持久化的高水位；否则一次
    // 已保留但未落成事件的序号会在增量迁移后被新事件复用。
    for (index, event) in ordered_events.iter_mut().enumerate() {
        let offset = u64::try_from(index)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| SessionStoreError::InvalidStore("会话事件序号已经耗尽。".to_string()))?;
        event.commit_seq = previous_runtime_manifest
            .last_commit_seq
            .checked_add(offset)
            .ok_or_else(|| SessionStoreError::InvalidStore("会话事件序号已经耗尽。".to_string()))?;
    }
    let last_commit_seq = ordered_events
        .last()
        .map(|event| event.commit_seq)
        .unwrap_or(0);
    let mut grouped = BTreeMap::<String, Vec<SessionEventV3>>::new();
    for event in ordered_events {
        grouped
            .entry(event.conversation_id.clone())
            .or_default()
            .push(event);
    }
    for events in grouped.values_mut() {
        events.sort_by_key(|event| event.commit_seq);
    }

    let staging_dir = sessions_dir.join("generations").join(format!(
        ".staging-{}-{}",
        current_migration_id,
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let conversations_dir = staging_dir.join("conversations");
    ensure_owned_directory(sessions_dir, &conversations_dir).await?;
    let mut conversation_index = BTreeMap::new();
    for (conversation_id, events) in &grouped {
        let file_name = conversation_file_name(conversation_id);
        conversation_index.insert(conversation_id.clone(), file_name.clone());
        write_json_lines_synced(&conversations_dir.join(file_name), events).await?;
    }
    let staging_quarantine = staging_dir.join("quarantine.jsonl");
    if !quarantined.is_empty() {
        write_json_lines_synced(&staging_quarantine, &quarantined).await?;
    }

    let migrated_records = grouped
        .values()
        .flat_map(|events| events.iter())
        .filter(|event| event.legacy_record.is_some())
        .count();
    let manifest = GenerationManifest {
        schema_version: SESSION_STORE_SCHEMA_VERSION.to_string(),
        generation_id: generation_id.clone(),
        migration_id: current_migration_id.clone(),
        created_at: Utc::now().to_rfc3339(),
        legacy_sources: sources.len(),
        source_records,
        migrated_records,
        deduplicated_records,
        quarantined_records: quarantined.len(),
        last_commit_seq,
        conversations: conversation_index,
        legacy_semantic_counts,
    };
    write_json_synced(&staging_dir.join("manifest.json"), &manifest).await?;
    let runtime_manifest = RuntimeManifest::from_generation_manifest(&manifest);
    write_json_synced(
        &staging_dir.join("runtime-manifest.json"),
        &runtime_manifest,
    )
    .await?;
    sync_generation_staging(&staging_dir).await?;
    validate_generation_for_publication(&staging_dir, &generation_id).await?;
    rename_path(&staging_dir, &generation_dir).await?;
    activate_generation(
        sessions_dir,
        &generation_id,
        Some(pointer.active_generation.clone()),
    )
    .await?;

    Ok(Some(MigrationOutput {
        report: SessionMigrationReport {
            migration_id: current_migration_id,
            generation_id,
            legacy_sources: manifest.legacy_sources,
            source_records: manifest.source_records,
            migrated_records: manifest.migrated_records,
            deduplicated_records: manifest.deduplicated_records,
            quarantined_records: manifest.quarantined_records,
            reused_existing_store: false,
            backup_dir: (!sources.is_empty()).then_some(backup_dir),
        },
        generation_dir,
        runtime_manifest,
    }))
}

async fn migrate_legacy_sources(sessions_dir: &Path) -> Result<MigrationOutput, SessionStoreError> {
    let sources = discover_legacy_sources(sessions_dir).await?;
    let migration_id = migration_id(&sources);
    let generation_id = format!("gen-{migration_id}");
    let generations_dir = sessions_dir.join("generations");
    ensure_owned_directory(sessions_dir, &generations_dir).await?;
    let generation_dir = generations_dir.join(&generation_id);
    let backup_dir = sessions_dir.join("backups").join(&migration_id);
    let report_backup_dir = (!sources.is_empty()).then(|| backup_dir.clone());

    if path_exists(&generation_dir).await? {
        validate_owned_directory(sessions_dir, &generation_dir).await?;
        backup_sources(&sources, sessions_dir, &backup_dir).await?;
        let manifest = load_manifest(&generation_dir).await?;
        if manifest.migration_id != migration_id {
            return Err(SessionStoreError::InvalidStore(format!(
                "generation `{generation_id}` 的迁移标识不匹配。"
            )));
        }
        if manifest.generation_id != generation_id {
            return Err(SessionStoreError::InvalidStore(format!(
                "generation 目录 `{generation_id}` 与清单标识 `{}` 不一致。",
                manifest.generation_id
            )));
        }
        validate_generation_for_publication(&generation_dir, &generation_id).await?;
        activate_generation(sessions_dir, &generation_id, None).await?;
        let runtime_manifest = load_runtime_manifest(&generation_dir).await?;
        return Ok(MigrationOutput {
            report: SessionMigrationReport {
                migration_id,
                generation_id,
                legacy_sources: manifest.legacy_sources,
                source_records: manifest.source_records,
                migrated_records: manifest.migrated_records,
                deduplicated_records: manifest.deduplicated_records,
                quarantined_records: manifest.quarantined_records,
                reused_existing_store: true,
                backup_dir: report_backup_dir,
            },
            generation_dir,
            runtime_manifest,
        });
    }

    backup_sources(&sources, sessions_dir, &backup_dir).await?;
    let mut records = Vec::new();
    let mut quarantined = Vec::new();
    let mut source_records = 0usize;
    for (source_order, source) in sources.iter().enumerate() {
        parse_source(
            source,
            source_order,
            &mut records,
            &mut quarantined,
            &mut source_records,
        );
    }

    let mut legacy_semantic_counts = BTreeMap::new();
    add_semantic_counts(&mut legacy_semantic_counts, &records);
    let (kept_records, deduplicated_records) = merge_legacy_mirrors(records);

    let staging_dir = generations_dir.join(format!(
        ".staging-{migration_id}-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let conversations_dir = staging_dir.join("conversations");
    ensure_owned_directory(sessions_dir, &conversations_dir).await?;
    let mut grouped = BTreeMap::<String, Vec<SessionEventV3>>::new();
    for (index, record) in kept_records.into_iter().enumerate() {
        let commit_seq = (index as u64).saturating_add(1);
        let event_id = hash_text(&format!(
            "{migration_id}\0{}\0{}\0{}",
            record.source_relative_path.display(),
            record.byte_offset,
            record.raw_hash
        ));
        let turn_outcome = terminal_outcome_for_kind(&record.kind);
        let legacy_source = legacy_event_source(&record);
        grouped
            .entry(record.conversation_id.clone())
            .or_default()
            .push(SessionEventV3 {
                schema_version: SESSION_EVENT_SCHEMA_VERSION.to_string(),
                event_id,
                commit_seq,
                conversation_id: record.conversation_id,
                turn_id: record.turn_id,
                kind: record.kind,
                time: record
                    .time
                    .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string()),
                payload: record.payload,
                turn_outcome,
                legacy_record: Some(record.value),
                legacy_source: Some(legacy_source),
            });
    }

    let mut conversation_index = BTreeMap::new();
    for (conversation_id, events) in &grouped {
        let file_name = conversation_file_name(conversation_id);
        conversation_index.insert(conversation_id.clone(), file_name.clone());
        write_json_lines_synced(&conversations_dir.join(file_name), events).await?;
    }
    if !quarantined.is_empty() {
        write_json_lines_synced(&staging_dir.join("quarantine.jsonl"), &quarantined).await?;
    }

    let last_commit_seq = grouped
        .values()
        .flat_map(|events| events.iter().map(|event| event.commit_seq))
        .max()
        .unwrap_or(0);
    let migrated_records = grouped.values().map(Vec::len).sum();
    let manifest = GenerationManifest {
        schema_version: SESSION_STORE_SCHEMA_VERSION.to_string(),
        generation_id: generation_id.clone(),
        migration_id: migration_id.clone(),
        created_at: Utc::now().to_rfc3339(),
        legacy_sources: sources.len(),
        source_records,
        migrated_records,
        deduplicated_records,
        quarantined_records: quarantined.len(),
        last_commit_seq,
        conversations: conversation_index,
        legacy_semantic_counts,
    };
    write_json_synced(&staging_dir.join("manifest.json"), &manifest).await?;
    let runtime_manifest = RuntimeManifest::from_generation_manifest(&manifest);
    write_json_synced(
        &staging_dir.join("runtime-manifest.json"),
        &runtime_manifest,
    )
    .await?;
    sync_generation_staging(&staging_dir).await?;
    validate_generation_for_publication(&staging_dir, &generation_id).await?;
    rename_path(&staging_dir, &generation_dir).await?;
    activate_generation(sessions_dir, &generation_id, None).await?;

    Ok(MigrationOutput {
        report: SessionMigrationReport {
            migration_id,
            generation_id,
            legacy_sources: sources.len(),
            source_records,
            migrated_records,
            deduplicated_records,
            quarantined_records: quarantined.len(),
            reused_existing_store: false,
            backup_dir: report_backup_dir,
        },
        generation_dir,
        runtime_manifest,
    })
}

async fn discover_legacy_sources(
    sessions_dir: &Path,
) -> Result<Vec<LegacySource>, SessionStoreError> {
    let mut candidates = Vec::<(PathBuf, PathBuf, LegacySourceKind)>::new();
    let global = sessions_dir.join("runtime.jsonl");
    match fs::symlink_metadata(&global).await {
        Ok(metadata) => {
            if metadata_is_link_or_reparse(&metadata) || !metadata.is_file() {
                return Err(SessionStoreError::InvalidStore(format!(
                    "拒绝迁移非普通文件 `{}`。",
                    global.display()
                )));
            }
            candidates.push((
                global,
                PathBuf::from("runtime.jsonl"),
                LegacySourceKind::Global,
            ));
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(io_error("检查", &global, source)),
    }

    let conversations_dir = sessions_dir.join("conversations");
    match fs::symlink_metadata(&conversations_dir).await {
        Ok(_) => {
            validate_owned_directory(sessions_dir, &conversations_dir).await?;
            let mut reader = fs::read_dir(&conversations_dir)
                .await
                .map_err(|source| io_error("读取目录", &conversations_dir, source))?;
            while let Some(entry) = reader
                .next_entry()
                .await
                .map_err(|source| io_error("遍历目录", &conversations_dir, source))?
            {
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                    continue;
                }
                let file_type = entry
                    .file_type()
                    .await
                    .map_err(|source| io_error("检查", &path, source))?;
                if file_type.is_symlink() || !file_type.is_file() {
                    return Err(SessionStoreError::InvalidStore(format!(
                        "拒绝迁移非普通文件 `{}`。",
                        path.display()
                    )));
                }
                let file_name = path.file_name().map(ToOwned::to_owned).ok_or_else(|| {
                    SessionStoreError::InvalidStore(format!(
                        "旧会话文件 `{}` 缺少文件名。",
                        path.display()
                    ))
                })?;
                candidates.push((
                    path,
                    PathBuf::from("conversations").join(file_name),
                    LegacySourceKind::Conversation,
                ));
            }
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(io_error("检查", &conversations_dir, source)),
    }
    candidates.sort_by(|left, right| left.1.cmp(&right.1));

    let mut sources = Vec::with_capacity(candidates.len());
    for (path, relative_path, kind) in candidates {
        let metadata = fs::symlink_metadata(&path)
            .await
            .map_err(|source| io_error("检查", &path, source))?;
        if metadata_is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err(SessionStoreError::InvalidStore(format!(
                "拒绝迁移非普通文件 `{}`。",
                path.display()
            )));
        }
        let bytes = read_regular_file_no_follow(&path).await?;
        sources.push(LegacySource {
            path,
            relative_path,
            kind,
            bytes,
            base_byte_offset: 0,
        });
    }
    Ok(sources)
}

fn migration_id(sources: &[LegacySource]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(MIGRATOR_VERSION.as_bytes());
    for source in sources {
        hasher.update([0]);
        hasher.update(source.relative_path.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update((source.bytes.len() as u64).to_le_bytes());
        hasher.update(Sha256::digest(&source.bytes));
    }
    format_hash(hasher.finalize().as_slice())
}

async fn backup_sources(
    sources: &[LegacySource],
    sessions_dir: &Path,
    backup_dir: &Path,
) -> Result<(), SessionStoreError> {
    if sources.is_empty() {
        return Ok(());
    }
    ensure_owned_directory(sessions_dir, backup_dir).await?;
    let mut synced_directories = BTreeSet::new();
    for source in sources {
        let destination = backup_dir.join(&source.relative_path);
        if let Some(parent) = destination.parent() {
            ensure_owned_directory(sessions_dir, parent).await?;
        }
        publish_backup_bytes(&destination, &source.bytes).await?;
        if let Some(parent) = destination.parent() {
            synced_directories.insert(parent.to_path_buf());
        }
    }
    for directory in synced_directories {
        sync_directory(&directory).await?;
    }
    if path_exists(backup_dir).await? {
        sync_directory(backup_dir).await?;
        if let Some(parent) = backup_dir.parent() {
            sync_directory(parent).await?;
        }
    }
    Ok(())
}

async fn publish_backup_bytes(
    destination: &Path,
    expected: &[u8],
) -> Result<(), SessionStoreError> {
    let parent = destination.parent().ok_or_else(|| {
        SessionStoreError::InvalidStore(format!(
            "备份目标 `{}` 缺少父目录。",
            destination.display()
        ))
    })?;
    create_dir_all(parent).await?;
    cleanup_stale_backup_temps(destination).await?;
    if path_exists(destination).await? {
        return verify_existing_backup(destination, expected).await;
    }

    let temporary = backup_temporary_path(destination);
    let mut file =
        open_regular_file_no_follow(&temporary, SecureFileMode::CreateNew, "创建备份临时文件")?;
    file.write_all(expected)
        .await
        .map_err(|source| io_error("写入备份临时文件", &temporary, source))?;
    file.flush()
        .await
        .map_err(|source| io_error("刷新备份临时文件", &temporary, source))?;
    file.sync_all()
        .await
        .map_err(|source| io_error("同步备份临时文件", &temporary, source))?;
    let staged = read_open_file_from_start(&mut file)
        .await
        .map_err(|source| io_error("回读备份临时文件", &temporary, source))?;
    if !same_backup_bytes(&staged, expected) {
        return Err(SessionStoreError::InvalidStore(format!(
            "备份临时文件 `{}` 的长度或 SHA-256 校验失败。",
            temporary.display()
        )));
    }
    drop(file);
    if path_exists(destination).await? {
        verify_existing_backup(destination, expected).await?;
        fs::remove_file(&temporary)
            .await
            .map_err(|source| io_error("清理备份临时文件", &temporary, source))?;
        sync_directory(parent).await?;
        return Ok(());
    }
    rename_path(&temporary, destination).await?;
    verify_existing_backup(destination, expected).await
}

async fn verify_existing_backup(
    destination: &Path,
    expected: &[u8],
) -> Result<(), SessionStoreError> {
    let mut file =
        open_regular_file_no_follow(destination, SecureFileMode::Read, "安全打开只读备份")?;
    let existing = read_open_file_from_start(&mut file)
        .await
        .map_err(|source| io_error("读取备份", destination, source))?;
    if !same_backup_bytes(&existing, expected) {
        return Err(SessionStoreError::InvalidStore(format!(
            "备份文件 `{}` 已存在但长度或 SHA-256 不一致，已停止迁移。",
            destination.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o400))
            .await
            .map_err(|source| io_error("设置只读备份权限", destination, source))?;
    }
    Ok(())
}

fn same_backup_bytes(actual: &[u8], expected: &[u8]) -> bool {
    actual.len() == expected.len() && Sha256::digest(actual) == Sha256::digest(expected)
}

fn backup_temporary_path(destination: &Path) -> PathBuf {
    let counter = BACKUP_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let prefix = backup_temporary_prefix(destination);
    destination
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            "{prefix}{}-{counter}.tmp",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
}

fn backup_temporary_prefix(destination: &Path) -> String {
    let file_name = destination
        .file_name()
        .map(|value| value.to_string_lossy())
        .unwrap_or_default();
    format!(".muse-backup-{}-", hash_text(&file_name))
}

async fn cleanup_stale_backup_temps(destination: &Path) -> Result<(), SessionStoreError> {
    let Some(parent) = destination.parent() else {
        return Ok(());
    };
    let prefix = backup_temporary_prefix(destination);
    let mut reader = fs::read_dir(parent)
        .await
        .map_err(|source| io_error("读取备份目录", parent, source))?;
    let mut removed = false;
    while let Some(entry) = reader
        .next_entry()
        .await
        .map_err(|source| io_error("遍历备份目录", parent, source))?
    {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(&prefix) || !name.ends_with(".tmp") {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .await
            .map_err(|source| io_error("检查备份临时文件", &path, source))?;
        if metadata.is_file() && !metadata.file_type().is_symlink() {
            fs::remove_file(&path)
                .await
                .map_err(|source| io_error("清理备份临时文件", &path, source))?;
            removed = true;
        }
    }
    if removed {
        sync_directory(parent).await?;
    }
    Ok(())
}

fn parse_source(
    source: &LegacySource,
    source_order: usize,
    records: &mut Vec<LegacyRecord>,
    quarantined: &mut Vec<QuarantineRecord>,
    source_records: &mut usize,
) {
    let mut byte_offset = source.base_byte_offset;
    for raw_with_newline in source.bytes.split_inclusive(|byte| *byte == b'\n') {
        let raw = raw_with_newline
            .strip_suffix(b"\n")
            .unwrap_or(raw_with_newline);
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        if raw.iter().all(u8::is_ascii_whitespace) {
            byte_offset = byte_offset.saturating_add(raw_with_newline.len());
            continue;
        }
        *source_records = source_records.saturating_add(1);
        match serde_json::from_slice::<Value>(raw) {
            Ok(value) if value.is_object() => {
                records.push(legacy_record_from_value(
                    source,
                    source_order,
                    byte_offset,
                    raw,
                    value,
                ));
            }
            Ok(_) => quarantined.push(QuarantineRecord {
                source_path: source.relative_path.to_string_lossy().into_owned(),
                byte_offset,
                raw_hex: encode_hex(raw),
                error: "旧记录顶层必须是 JSON 对象。".to_string(),
            }),
            Err(error) => quarantined.push(QuarantineRecord {
                source_path: source.relative_path.to_string_lossy().into_owned(),
                byte_offset,
                raw_hex: encode_hex(raw),
                error: format!("旧记录 JSON 解析失败：{error}"),
            }),
        }
        byte_offset = byte_offset.saturating_add(raw_with_newline.len());
    }
}

fn legacy_record_from_value(
    source: &LegacySource,
    source_order: usize,
    byte_offset: usize,
    raw: &[u8],
    value: Value,
) -> LegacyRecord {
    let payload = value.get("payload").cloned().unwrap_or(Value::Null);
    let conversation_id = string_field(&payload, "conversation_id")
        .or_else(|| string_field(&value, "conversation_id"))
        .or_else(|| {
            (source.kind == LegacySourceKind::Conversation)
                .then(|| source.path.file_stem()?.to_str().map(ToString::to_string))
                .flatten()
        })
        .unwrap_or_else(|| DEFAULT_CONVERSATION_ID.to_string());
    let turn_id = string_field(&payload, "turn_id").or_else(|| string_field(&value, "turn_id"));
    let kind = string_field(&value, "kind").unwrap_or_else(|| "legacy".to_string());
    let time = string_field(&value, "time")
        .or_else(|| string_field(&value, "timestamp"))
        .or_else(|| string_field(&payload, "updated_at"));
    let semantic_bytes = serde_json::to_vec(&value).unwrap_or_else(|_| raw.to_vec());
    LegacyRecord {
        source_relative_path: source.relative_path.clone(),
        source_kind: source.kind,
        source_order,
        byte_offset,
        value,
        conversation_id,
        turn_id,
        kind,
        time,
        payload,
        semantic_hash: hash_bytes(&semantic_bytes),
        raw_hash: hash_bytes(raw),
    }
}

/// 将全局 transcript 与分会话 transcript 按语义指纹和各自源顺序稳定合并。
///
/// 全局流作为跨会话顺序骨架；分会话流通过最长公共子序列寻找真实镜像锚点，
/// 单侧独有记录插入相邻锚点之间。这样 `A,C` 与 `A,B,C` 会合并为
/// `A,B,C`，合法重复也按出现次序逐个配对，而不是按内容集合粗暴删除。
fn merge_legacy_mirrors(records: Vec<LegacyRecord>) -> (Vec<LegacyRecord>, usize) {
    let mut global = records
        .iter()
        .filter(|record| record.source_kind == LegacySourceKind::Global)
        .cloned()
        .collect::<Vec<_>>();
    global.sort_by(|left, right| {
        left.source_order
            .cmp(&right.source_order)
            .then_with(|| left.byte_offset.cmp(&right.byte_offset))
    });

    let mut per_conversation = BTreeMap::<String, Vec<LegacyRecord>>::new();
    for record in records {
        if record.source_kind == LegacySourceKind::Conversation {
            per_conversation
                .entry(record.conversation_id.clone())
                .or_default()
                .push(record);
        }
    }
    for records in per_conversation.values_mut() {
        records.sort_by(|left, right| {
            left.source_order
                .cmp(&right.source_order)
                .then_with(|| left.byte_offset.cmp(&right.byte_offset))
        });
    }

    let mut insertions = BTreeMap::<usize, Vec<LegacyRecord>>::new();
    let mut deduplicated = 0usize;
    for (conversation_id, conversation_records) in per_conversation {
        let global_indices = global
            .iter()
            .enumerate()
            .filter_map(|(index, record)| {
                (record.conversation_id == conversation_id).then_some(index)
            })
            .collect::<Vec<_>>();
        let global_records = global_indices
            .iter()
            .map(|index| &global[*index])
            .collect::<Vec<_>>();
        let matches = legacy_lcs_matches(&global_records, &conversation_records);
        deduplicated = deduplicated.saturating_add(matches.len());

        let mut previous_conversation_index = 0usize;
        for (global_index_in_conversation, conversation_index) in matches {
            let insertion_index = global_indices[global_index_in_conversation];
            insertions.entry(insertion_index).or_default().extend(
                conversation_records[previous_conversation_index..conversation_index]
                    .iter()
                    .cloned(),
            );
            previous_conversation_index = conversation_index.saturating_add(1);
        }
        insertions.entry(global.len()).or_default().extend(
            conversation_records[previous_conversation_index..]
                .iter()
                .cloned(),
        );
    }

    for records in insertions.values_mut() {
        records.sort_by(|left, right| {
            left.source_order
                .cmp(&right.source_order)
                .then_with(|| left.byte_offset.cmp(&right.byte_offset))
        });
    }

    let global_len = global.len();
    let mut merged = Vec::with_capacity(
        global_len.saturating_add(insertions.values().map(Vec::len).sum::<usize>()),
    );
    for (index, record) in global.into_iter().enumerate() {
        if let Some(records) = insertions.remove(&index) {
            merged.extend(records);
        }
        merged.push(record);
    }
    if let Some(records) = insertions.remove(&global_len) {
        merged.extend(records);
    }
    (merged, deduplicated)
}

/// Hunt-Szymanski 形式的 LCS：对话侧逐条处理、全局侧相同指纹位置倒序更新，
/// 在避免二次方矩阵内存的同时正确处理合法重复事件。
fn legacy_lcs_matches(
    global: &[&LegacyRecord],
    conversation: &[LegacyRecord],
) -> Vec<(usize, usize)> {
    let global_hashes = global
        .iter()
        .map(|record| record.semantic_hash.clone())
        .collect::<Vec<_>>();
    let conversation_hashes = conversation
        .iter()
        .map(|record| record.semantic_hash.clone())
        .collect::<Vec<_>>();
    semantic_lcs_matches(&global_hashes, &conversation_hashes)
}

fn semantic_lcs_matches(left: &[String], right: &[String]) -> Vec<(usize, usize)> {
    #[derive(Clone, Copy)]
    struct MatchNode {
        left_index: usize,
        right_index: usize,
        previous: Option<usize>,
    }

    let mut positions = HashMap::<&str, Vec<usize>>::new();
    for (index, hash) in left.iter().enumerate() {
        positions.entry(hash.as_str()).or_default().push(index);
    }

    let mut tail_left_indices = Vec::<usize>::new();
    let mut tail_nodes = Vec::<usize>::new();
    let mut nodes = Vec::<MatchNode>::new();
    for (right_index, hash) in right.iter().enumerate() {
        let Some(candidates) = positions.get(hash.as_str()) else {
            continue;
        };
        for &left_index in candidates.iter().rev() {
            let length = tail_left_indices.partition_point(|value| *value < left_index);
            let previous = length
                .checked_sub(1)
                .and_then(|index| tail_nodes.get(index).copied());
            let node_index = nodes.len();
            nodes.push(MatchNode {
                left_index,
                right_index,
                previous,
            });
            if length == tail_left_indices.len() {
                tail_left_indices.push(left_index);
                tail_nodes.push(node_index);
            } else if left_index < tail_left_indices[length] {
                // 相同 left 锚点优先保留更早出现的 right 记录；否则增量追加的
                // 合法重复会被误当成旧锚点，错误插到 native 事件之前。
                tail_left_indices[length] = left_index;
                tail_nodes[length] = node_index;
            }
        }
    }

    let Some(mut node_index) = tail_nodes.last().copied() else {
        return Vec::new();
    };
    let mut matches = Vec::with_capacity(tail_nodes.len());
    loop {
        let node = nodes[node_index];
        matches.push((node.left_index, node.right_index));
        let Some(previous) = node.previous else {
            break;
        };
        node_index = previous;
    }
    matches.reverse();
    matches
}

fn semantic_count_key(record: &LegacyRecord) -> String {
    hash_text(&format!(
        "{}\0{}",
        record.conversation_id, record.semantic_hash
    ))
}

fn legacy_value_semantic_hash(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_else(|_| value.to_string().into_bytes());
    hash_bytes(&bytes)
}

fn add_semantic_counts(
    counts: &mut BTreeMap<String, LegacySemanticCounts>,
    records: &[LegacyRecord],
) {
    for record in records {
        let entry = counts.entry(semantic_count_key(record)).or_default();
        match record.source_kind {
            LegacySourceKind::Global => entry.global = entry.global.saturating_add(1),
            LegacySourceKind::Conversation => {
                entry.conversation = entry.conversation.saturating_add(1);
            }
        }
    }
}

fn legacy_event_source(record: &LegacyRecord) -> LegacyEventSource {
    LegacyEventSource {
        relative_path: record.source_relative_path.to_string_lossy().into_owned(),
        source_kind: match record.source_kind {
            LegacySourceKind::Global => "global",
            LegacySourceKind::Conversation => "conversation",
        }
        .to_string(),
        byte_offset: record.byte_offset,
    }
}

fn terminal_outcome_for_kind(kind: &str) -> Option<String> {
    match kind {
        "turn_committed" => Some("committed".to_string()),
        "turn_aborted" => Some("aborted".to_string()),
        "turn_interrupted_with_effects" => Some("interrupted_with_effects".to_string()),
        _ => None,
    }
}

fn shared_writer(
    generation_dir: &Path,
    runtime_manifest: RuntimeManifest,
) -> Arc<Mutex<WriterState>> {
    let registry = WRITER_REGISTRY.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut registry = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.retain(|_, writer| writer.strong_count() > 0);
    if let Some(writer) = registry.get(generation_dir).and_then(Weak::upgrade) {
        return writer;
    }
    let writer = Arc::new(Mutex::new(WriterState {
        next_commit_seq: runtime_manifest.last_commit_seq.saturating_add(1),
        runtime_manifest,
        poison_reason: None,
    }));
    registry.insert(generation_dir.to_path_buf(), Arc::downgrade(&writer));
    writer
}

async fn activate_generation(
    sessions_dir: &Path,
    active_generation: &str,
    previous_generation: Option<String>,
) -> Result<(), SessionStoreError> {
    if !safe_generation_id(active_generation) {
        return Err(SessionStoreError::InvalidStore(format!(
            "generation 标识 `{active_generation}` 不安全。"
        )));
    }
    let pointer = StorePointer {
        schema_version: SESSION_STORE_SCHEMA_VERSION.to_string(),
        active_generation: active_generation.to_string(),
        previous_generation,
        updated_at: Utc::now().to_rfc3339(),
    };
    let pointer_path = sessions_dir.join("store.json");
    let temporary_path = sessions_dir.join(format!(
        ".store-{}.tmp",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    write_json_synced(&temporary_path, &pointer).await?;
    replace_file_path(&temporary_path, &pointer_path).await?;
    sync_directory(sessions_dir).await
}

#[cfg(not(windows))]
async fn replace_file_path(
    source_path: &Path,
    destination: &Path,
) -> Result<(), SessionStoreError> {
    rename_path(source_path, destination).await
}

#[cfg(windows)]
async fn replace_file_path(
    source_path: &Path,
    destination: &Path,
) -> Result<(), SessionStoreError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source = source_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: 两个 UTF-16 缓冲区均以 NUL 结尾并在调用期间保持有效；标志只请求
    // 原子替换现有指针并在返回前刷新文件系统元数据。
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        return Err(SessionStoreError::Io {
            operation: "原子替换",
            path: destination.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(())
}

fn validate_pointer(pointer: &StorePointer) -> Result<(), SessionStoreError> {
    if pointer.schema_version != SESSION_STORE_SCHEMA_VERSION {
        return Err(SessionStoreError::InvalidStore(format!(
            "不支持会话存储版本 `{}`。",
            pointer.schema_version
        )));
    }
    if !safe_generation_id(&pointer.active_generation) {
        return Err(SessionStoreError::InvalidStore(format!(
            "generation 标识 `{}` 不安全。",
            pointer.active_generation
        )));
    }
    Ok(())
}

fn safe_generation_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

async fn load_manifest(generation_dir: &Path) -> Result<GenerationManifest, SessionStoreError> {
    let manifest_path = generation_dir.join("manifest.json");
    let manifest: GenerationManifest = read_json(&manifest_path).await?;
    if manifest.schema_version != SESSION_STORE_SCHEMA_VERSION {
        return Err(SessionStoreError::InvalidStore(format!(
            "generation `{}` 使用不支持的版本 `{}`。",
            generation_dir.display(),
            manifest.schema_version
        )));
    }
    if !safe_generation_id(&manifest.generation_id) {
        return Err(SessionStoreError::InvalidStore(format!(
            "generation 清单标识 `{}` 不安全。",
            manifest.generation_id
        )));
    }
    if !is_sha256_hex(&manifest.migration_id) {
        return Err(SessionStoreError::InvalidStore(format!(
            "generation `{}` 的迁移标识不是有效 SHA-256。",
            manifest.generation_id
        )));
    }
    validate_conversation_file_index("generation 清单", &manifest.conversations)?;
    Ok(manifest)
}

async fn load_runtime_manifest(
    generation_dir: &Path,
) -> Result<RuntimeManifest, SessionStoreError> {
    let path = generation_dir.join("runtime-manifest.json");
    let manifest: RuntimeManifest = read_json(&path).await?;
    if manifest.schema_version != RUNTIME_MANIFEST_SCHEMA_VERSION {
        return Err(SessionStoreError::InvalidStore(format!(
            "活动 generation `{}` 使用不支持的运行清单版本 `{}`。",
            generation_dir.display(),
            manifest.schema_version
        )));
    }
    if !safe_generation_id(&manifest.generation_id) {
        return Err(SessionStoreError::InvalidStore(format!(
            "活动 generation 清单标识 `{}` 不安全。",
            manifest.generation_id
        )));
    }
    validate_conversation_file_index("运行清单", &manifest.conversations)?;
    validate_conversation_file_index("运行清单待删除事务", &manifest.pending_deletions)?;
    if let Some(conversation_id) = manifest
        .pending_conversations
        .iter()
        .find(|conversation_id| conversation_id.trim().is_empty())
    {
        return Err(SessionStoreError::InvalidStore(format!(
            "运行清单包含空会话标识 `{conversation_id}`。"
        )));
    }
    if let Some(conversation_id) = manifest
        .pending_conversations
        .iter()
        .find(|conversation_id| !manifest.conversations.contains_key(*conversation_id))
    {
        return Err(SessionStoreError::InvalidStore(format!(
            "运行清单待完成会话 `{conversation_id}` 缺少对应索引。"
        )));
    }
    if let Some(conversation_id) = manifest
        .pending_deletions
        .keys()
        .find(|conversation_id| manifest.conversations.contains_key(*conversation_id))
    {
        return Err(SessionStoreError::InvalidStore(format!(
            "运行清单会话 `{conversation_id}` 同时处于活动索引和待删除事务。"
        )));
    }
    Ok(manifest)
}

fn validate_conversation_file_index(
    owner: &str,
    conversations: &BTreeMap<String, String>,
) -> Result<(), SessionStoreError> {
    let mut file_names = BTreeSet::new();
    for (conversation_id, file_name) in conversations {
        if conversation_id.trim().is_empty() {
            return Err(SessionStoreError::InvalidStore(format!(
                "{owner}包含空 conversation_id。"
            )));
        }
        let expected = conversation_file_name(conversation_id);
        if file_name != &expected {
            return Err(SessionStoreError::InvalidStore(format!(
                "{owner}中会话 `{conversation_id}` 的文件名 `{file_name}` 与完整 ID 的 SHA-256 不一致。"
            )));
        }
        if !file_names.insert(file_name) {
            return Err(SessionStoreError::InvalidStore(format!(
                "{owner}重复引用会话文件 `{file_name}`。"
            )));
        }
    }
    Ok(())
}

/// 兼容本治理版本发布前已经存在的活动 generation，并完成所有明确可判定的
/// 崩溃中间态。不能证明安全的数据缺失会直接拒绝启动，而不是猜测修复。
async fn load_or_rebuild_runtime_manifest(
    generation_dir: &Path,
    generation_manifest: &GenerationManifest,
) -> Result<RuntimeManifest, SessionStoreError> {
    let runtime_path = generation_dir.join("runtime-manifest.json");
    let mut runtime_manifest = if path_exists(&runtime_path).await? {
        load_runtime_manifest(generation_dir).await?
    } else {
        RuntimeManifest::from_generation_manifest(generation_manifest)
    };
    if runtime_manifest.generation_id != generation_manifest.generation_id {
        return Err(SessionStoreError::InvalidStore(format!(
            "运行清单 generation `{}` 与迁移清单 `{}` 不一致。",
            runtime_manifest.generation_id, generation_manifest.generation_id
        )));
    }

    let conversations_dir = generation_dir.join("conversations");
    let mut changed = !path_exists(&runtime_path).await?;

    // 删除在清单中先记账；启动时只完成有明确 pending 标识的删除。
    for (conversation_id, file_name) in runtime_manifest.pending_deletions.clone() {
        let path = conversations_dir.join(&file_name);
        if path_exists(&path).await? {
            fs::remove_file(&path)
                .await
                .map_err(|source| io_error("完成待处理删除", &path, source))?;
            sync_directory(&conversations_dir).await?;
        }
        runtime_manifest.pending_deletions.remove(&conversation_id);
        changed = true;
    }

    let mut actual_files = conversation_file_names(&conversations_dir).await?;
    // 首条事件尚未落盘时，只有 pending_conversations 中的索引允许回退。
    for conversation_id in runtime_manifest.pending_conversations.clone() {
        let expected_file = conversation_file_name(&conversation_id);
        if actual_files.contains(&expected_file) {
            runtime_manifest
                .pending_conversations
                .remove(&conversation_id);
        } else {
            runtime_manifest.conversations.remove(&conversation_id);
            runtime_manifest
                .pending_conversations
                .remove(&conversation_id);
        }
        changed = true;
    }

    let indexed_files = runtime_manifest
        .conversations
        .values()
        .cloned()
        .collect::<BTreeSet<_>>();
    if let Some(missing) = indexed_files.difference(&actual_files).next() {
        return Err(SessionStoreError::InvalidStore(format!(
            "运行清单引用的会话文件 `{missing}` 丢失且不属于可恢复的首条追加。"
        )));
    }

    // 旧版本运行期追加没有活动清单。对磁盘上的额外完整文件，从首个事件恢复
    // 会话索引；空文件没有足够证据，必须拒绝猜测其所属会话。
    for file_name in actual_files.difference(&indexed_files) {
        let path = conversations_dir.join(file_name);
        let events = read_event_file(&path).await?;
        let conversation_id = events
            .first()
            .map(|event| event.conversation_id.clone())
            .ok_or_else(|| {
                SessionStoreError::InvalidStore(format!(
                    "未索引的空会话文件 `{file_name}` 无法安全恢复。"
                ))
            })?;
        if conversation_file_name(&conversation_id) != *file_name {
            return Err(SessionStoreError::InvalidStore(format!(
                "未索引会话文件 `{file_name}` 与其中 conversation_id 的哈希不一致。"
            )));
        }
        runtime_manifest
            .conversations
            .insert(conversation_id, file_name.clone());
        changed = true;
    }
    actual_files = conversation_file_names(&conversations_dir).await?;
    debug_assert_eq!(
        actual_files,
        runtime_manifest
            .conversations
            .values()
            .cloned()
            .collect::<BTreeSet<_>>()
    );

    let disk_max = scan_max_commit_sequence(generation_dir).await?;
    let reconciled_high_watermark = runtime_manifest
        .last_commit_seq
        .max(generation_manifest.last_commit_seq)
        .max(disk_max);
    if runtime_manifest.last_commit_seq != reconciled_high_watermark {
        runtime_manifest.last_commit_seq = reconciled_high_watermark;
        changed = true;
    }
    if changed {
        runtime_manifest.updated_at = Utc::now().to_rfc3339();
        persist_runtime_manifest(generation_dir, &runtime_manifest).await?;
    }
    Ok(runtime_manifest)
}

async fn persist_runtime_manifest(
    generation_dir: &Path,
    manifest: &RuntimeManifest,
) -> Result<(), SessionStoreError> {
    let destination = generation_dir.join("runtime-manifest.json");
    let temporary = generation_dir.join(format!(
        ".runtime-manifest-{}-{}.tmp",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    write_json_synced(&temporary, manifest).await?;
    replace_file_path(&temporary, &destination).await?;
    sync_directory(generation_dir).await
}

/// 修复活动 transcript 的唯一可自动判定尾部：完整 JSON 但缺少换行时补换行；
/// 非法未完成尾部逐字节隔离后截断。带换行的坏行不属于崩溃半写，继续拒绝。
async fn repair_runtime_event_tails(
    sessions_dir: &Path,
    generation_dir: &Path,
) -> Result<(), SessionStoreError> {
    let conversations_dir = generation_dir.join("conversations");
    if !path_exists(&conversations_dir).await? {
        return Ok(());
    }
    validate_owned_directory(sessions_dir, generation_dir).await?;
    validate_owned_directory(sessions_dir, &conversations_dir).await?;
    for path in read_file_entries(&conversations_dir).await? {
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        let mut file = open_regular_file_no_follow(
            &path,
            SecureFileMode::ReadWrite,
            "安全打开活动 transcript",
        )?;
        let bytes = read_open_file_from_start(&mut file)
            .await
            .map_err(|source| io_error("读取", &path, source))?;
        if bytes.is_empty() || bytes.ends_with(b"\n") {
            continue;
        }
        let tail_start = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0usize, |index| index.saturating_add(1));
        let raw_tail = &bytes[tail_start..];
        let valid_complete_event = serde_json::from_slice::<SessionEventV3>(raw_tail)
            .ok()
            .is_some_and(|event| event.schema_version == SESSION_EVENT_SCHEMA_VERSION);
        if valid_complete_event {
            file.seek(std::io::SeekFrom::End(0))
                .await
                .map_err(|source| io_error("定位", &path, source))?;
            file.write_all(b"\n")
                .await
                .map_err(|source| io_error("补全换行", &path, source))?;
            file.flush()
                .await
                .map_err(|source| io_error("刷新", &path, source))?;
            file.sync_all()
                .await
                .map_err(|source| io_error("同步", &path, source))?;
            continue;
        }

        let quarantine = RuntimeQuarantineRecord {
            source_path: path
                .strip_prefix(generation_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned(),
            byte_offset: u64::try_from(tail_start).unwrap_or(u64::MAX),
            raw_hex: encode_hex(raw_tail),
            error: "活动 transcript 末尾是不完整或非法的无换行记录，已原样隔离。".to_string(),
            quarantined_at: Utc::now().to_rfc3339(),
        };
        let quarantine_path = generation_dir.join("runtime-quarantine.jsonl");
        let mut line =
            serde_json::to_vec(&quarantine).map_err(|source| SessionStoreError::InvalidJson {
                path: quarantine_path.clone(),
                source,
            })?;
        line.push(b'\n');
        append_bytes_with_recovery(&quarantine_path, &line)
            .await
            .map_err(|failure| failure.error)?;
        file.set_len(u64::try_from(tail_start).unwrap_or(u64::MAX))
            .await
            .map_err(|source| io_error("截断", &path, source))?;
        file.sync_all()
            .await
            .map_err(|source| io_error("同步", &path, source))?;
        sync_directory(&conversations_dir).await?;
    }
    Ok(())
}

/// 发布 generation 前从磁盘完整回读清单、事件和隔离记录。
///
/// 迁移过程中构造的内存对象不能作为发布依据；只有落盘内容通过全部不变量后，
/// 调用方才可以把 staging 目录改名并更新 `store.json`。
async fn validate_generation_for_publication(
    generation_dir: &Path,
    expected_generation_id: &str,
) -> Result<(), SessionStoreError> {
    let manifest = load_manifest(generation_dir).await?;
    let runtime_manifest = load_runtime_manifest(generation_dir).await?;
    if manifest.generation_id != expected_generation_id {
        return Err(SessionStoreError::InvalidStore(format!(
            "待发布 generation `{expected_generation_id}` 与清单标识 `{}` 不一致。",
            manifest.generation_id
        )));
    }
    if runtime_manifest.generation_id != expected_generation_id {
        return Err(SessionStoreError::InvalidStore(format!(
            "待发布 generation `{expected_generation_id}` 与运行清单标识 `{}` 不一致。",
            runtime_manifest.generation_id
        )));
    }
    if !runtime_manifest.pending_conversations.is_empty()
        || !runtime_manifest.pending_deletions.is_empty()
    {
        return Err(SessionStoreError::InvalidStore(
            "待发布 generation 仍包含未完成的运行期索引事务。".to_string(),
        ));
    }

    let conversations_dir = generation_dir.join("conversations");
    let actual_files = conversation_file_names(&conversations_dir).await?;
    let mut indexed_files = BTreeSet::new();
    for (conversation_id, file_name) in &runtime_manifest.conversations {
        validate_non_empty("conversation_id", conversation_id)?;
        let expected_file_name = conversation_file_name(conversation_id);
        if file_name != &expected_file_name {
            return Err(SessionStoreError::InvalidStore(format!(
                "会话 `{conversation_id}` 的清单文件名 `{file_name}` 与完整 ID 哈希不一致。"
            )));
        }
        if !indexed_files.insert(file_name.clone()) {
            return Err(SessionStoreError::InvalidStore(format!(
                "generation 清单重复引用会话文件 `{file_name}`。"
            )));
        }
    }
    if actual_files != indexed_files {
        return Err(SessionStoreError::InvalidStore(format!(
            "generation 会话文件与清单不一致：清单 {} 个，磁盘 {} 个。",
            indexed_files.len(),
            actual_files.len()
        )));
    }

    let mut event_ids = BTreeSet::new();
    let mut commit_sequences = BTreeSet::new();
    let mut all_sequences = Vec::new();
    let mut legacy_event_count = 0usize;
    for (conversation_id, file_name) in &runtime_manifest.conversations {
        let path = conversations_dir.join(file_name);
        let events = read_event_file(&path).await?;
        let mut previous_sequence = None;
        let mut tool_calls = BTreeSet::<(String, String)>::new();
        let mut tool_results = BTreeSet::<(String, String)>::new();
        for event in events {
            if event.conversation_id != *conversation_id {
                return Err(SessionStoreError::InvalidStore(format!(
                    "会话文件 `{file_name}` 包含其他会话 `{}` 的事件 `{}`。",
                    event.conversation_id, event.event_id
                )));
            }
            validate_non_empty("event_id", &event.event_id)?;
            if event.commit_seq == 0 {
                return Err(SessionStoreError::InvalidStore(format!(
                    "事件 `{}` 的 commit_seq 必须从 1 开始。",
                    event.event_id
                )));
            }
            if previous_sequence.is_some_and(|previous| event.commit_seq <= previous) {
                return Err(SessionStoreError::InvalidStore(format!(
                    "会话文件 `{file_name}` 的 commit_seq 未严格递增。"
                )));
            }
            previous_sequence = Some(event.commit_seq);
            if !event_ids.insert(event.event_id.clone()) {
                return Err(SessionStoreError::InvalidStore(format!(
                    "generation 存在重复 event_id `{}`。",
                    event.event_id
                )));
            }
            if !commit_sequences.insert(event.commit_seq) {
                return Err(SessionStoreError::InvalidStore(format!(
                    "generation 存在重复 commit_seq `{}`。",
                    event.commit_seq
                )));
            }
            all_sequences.push(event.commit_seq);
            legacy_event_count = legacy_event_count
                .checked_add(usize::from(event.legacy_record.is_some()))
                .ok_or_else(|| SessionStoreError::InvalidStore("会话事件计数溢出。".to_string()))?;

            if matches!(event.kind.as_str(), "tool_call" | "tool_result") {
                let turn_id = event
                    .turn_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        SessionStoreError::InvalidStore(format!(
                            "工具事件 `{}` 缺少 turn_id。",
                            event.event_id
                        ))
                    })?;
                let call_id = string_field(&event.payload, "call_id").ok_or_else(|| {
                    SessionStoreError::InvalidStore(format!(
                        "工具事件 `{}` 缺少 payload.call_id。",
                        event.event_id
                    ))
                })?;
                let key = (turn_id.to_string(), call_id.clone());
                if event.kind == "tool_call" {
                    if !tool_calls.insert(key) {
                        return Err(SessionStoreError::InvalidStore(format!(
                            "回合 `{turn_id}` 存在重复 tool_call `{call_id}`。"
                        )));
                    }
                } else {
                    if !tool_calls.contains(&key) {
                        return Err(SessionStoreError::InvalidStore(format!(
                            "tool_result `{call_id}` 缺少同回合 `{turn_id}` 的前置 tool_call。"
                        )));
                    }
                    if !tool_results.insert(key) {
                        return Err(SessionStoreError::InvalidStore(format!(
                            "回合 `{turn_id}` 存在重复 tool_result `{call_id}`。"
                        )));
                    }
                }
            }
        }
    }

    all_sequences.sort_unstable();
    if all_sequences.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(SessionStoreError::InvalidStore(
            "generation 的 commit_seq 未严格递增。".to_string(),
        ));
    }
    let actual_last_commit_seq = all_sequences.last().copied().unwrap_or(0);
    if runtime_manifest.last_commit_seq < actual_last_commit_seq
        || runtime_manifest.last_commit_seq < manifest.last_commit_seq
    {
        return Err(SessionStoreError::InvalidStore(format!(
            "generation 运行清单高水位为 {}，迁移基线为 {}，完整回读最大序号为 {actual_last_commit_seq}。",
            runtime_manifest.last_commit_seq, manifest.last_commit_seq
        )));
    }
    let accounted_legacy_events = legacy_event_count
        .checked_add(runtime_manifest.removed_legacy_records)
        .ok_or_else(|| SessionStoreError::InvalidStore("迁移事件计数溢出。".to_string()))?;
    if manifest.migrated_records != accounted_legacy_events {
        return Err(SessionStoreError::InvalidStore(format!(
            "generation 清单 migrated_records 为 {}，完整回读迁移事件数与已删除数合计为 {accounted_legacy_events}。",
            manifest.migrated_records,
        )));
    }

    let quarantine_path = generation_dir.join("quarantine.jsonl");
    let quarantine_count = if path_exists(&quarantine_path).await? {
        read_quarantine_file(&quarantine_path).await?.len()
    } else {
        0
    };
    if manifest.quarantined_records != quarantine_count {
        return Err(SessionStoreError::InvalidStore(format!(
            "generation 清单 quarantined_records 为 {}，完整回读结果为 {quarantine_count}。",
            manifest.quarantined_records
        )));
    }

    let accounted_source_records = manifest
        .migrated_records
        .checked_add(manifest.deduplicated_records)
        .and_then(|count| count.checked_add(manifest.quarantined_records))
        .ok_or_else(|| SessionStoreError::InvalidStore("迁移来源计数溢出。".to_string()))?;
    if manifest.source_records != accounted_source_records {
        return Err(SessionStoreError::InvalidStore(format!(
            "generation 清单来源计数不守恒：source_records={}，migrated+deduplicated+quarantined={accounted_source_records}。",
            manifest.source_records
        )));
    }

    let semantic_multiplicity = manifest
        .legacy_semantic_counts
        .values()
        .try_fold(0usize, |total, counts| {
            total.checked_add(counts.global.max(counts.conversation))
        })
        .ok_or_else(|| SessionStoreError::InvalidStore("迁移语义计数溢出。".to_string()))?;
    if semantic_multiplicity != manifest.migrated_records {
        return Err(SessionStoreError::InvalidStore(format!(
            "generation 清单语义计数为 {semantic_multiplicity}，migrated_records 为 {}。",
            manifest.migrated_records
        )));
    }

    Ok(())
}

async fn conversation_file_names(
    conversations_dir: &Path,
) -> Result<BTreeSet<String>, SessionStoreError> {
    let mut reader = fs::read_dir(conversations_dir)
        .await
        .map_err(|source| io_error("读取目录", conversations_dir, source))?;
    let mut names = BTreeSet::new();
    while let Some(entry) = reader
        .next_entry()
        .await
        .map_err(|source| io_error("遍历目录", conversations_dir, source))?
    {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .await
            .map_err(|source| io_error("检查", &path, source))?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(SessionStoreError::InvalidStore(format!(
                "generation 会话目录包含非普通文件 `{}`。",
                path.display()
            )));
        }
        let name = entry.file_name().into_string().map_err(|_| {
            SessionStoreError::InvalidStore(format!(
                "generation 会话目录包含非 UTF-8 文件名 `{}`。",
                path.display()
            ))
        })?;
        if !names.insert(name.clone()) {
            return Err(SessionStoreError::InvalidStore(format!(
                "generation 会话目录包含重复文件名 `{name}`。"
            )));
        }
    }
    Ok(names)
}

async fn scan_max_commit_sequence(generation_dir: &Path) -> Result<u64, SessionStoreError> {
    let conversations_dir = generation_dir.join("conversations");
    if !path_exists(&conversations_dir).await? {
        return Ok(0);
    }
    let mut max_sequence = 0u64;
    for path in read_file_entries(&conversations_dir).await? {
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        for event in read_event_file(&path).await? {
            max_sequence = max_sequence.max(event.commit_seq);
        }
    }
    Ok(max_sequence)
}

async fn read_event_file(path: &Path) -> Result<Vec<SessionEventV3>, SessionStoreError> {
    let bytes = read_regular_file_no_follow(path).await?;
    let mut events = Vec::new();
    for raw in bytes.split(|byte| *byte == b'\n') {
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let event = serde_json::from_slice::<SessionEventV3>(raw).map_err(|source| {
            SessionStoreError::InvalidJson {
                path: path.to_path_buf(),
                source,
            }
        })?;
        if event.schema_version != SESSION_EVENT_SCHEMA_VERSION {
            return Err(SessionStoreError::InvalidStore(format!(
                "事件 `{}` 使用不支持的版本 `{}`。",
                event.event_id, event.schema_version
            )));
        }
        events.push(event);
    }
    Ok(events)
}

async fn read_quarantine_file(path: &Path) -> Result<Vec<QuarantineRecord>, SessionStoreError> {
    let bytes = read_regular_file_no_follow(path).await?;
    let mut records = Vec::new();
    for raw in bytes.split(|byte| *byte == b'\n') {
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        records.push(serde_json::from_slice(raw).map_err(|source| {
            SessionStoreError::InvalidJson {
                path: path.to_path_buf(),
                source,
            }
        })?);
    }
    Ok(records)
}

async fn append_bytes_with_recovery(path: &Path, bytes: &[u8]) -> Result<(), AppendFailure> {
    let parent = path.parent().ok_or_else(|| AppendFailure {
        error: SessionStoreError::InvalidStore(format!(
            "追加目标 `{}` 缺少父目录。",
            path.display()
        )),
        poison_writer: false,
    })?;
    let parent_metadata = fs::symlink_metadata(parent)
        .await
        .map_err(|source| AppendFailure {
            error: io_error("检查目录", parent, source),
            poison_writer: false,
        })?;
    if metadata_is_link_or_reparse(&parent_metadata) || !parent_metadata.is_dir() {
        return Err(AppendFailure {
            error: SessionStoreError::InvalidStore(format!(
                "追加目标目录 `{}` 不是普通目录或包含符号链接。",
                parent.display()
            )),
            poison_writer: false,
        });
    }
    let existed = path_exists(path).await.map_err(|error| AppendFailure {
        error,
        poison_writer: false,
    })?;
    let mut file =
        open_regular_file_no_follow(path, SecureFileMode::AppendOrCreate, "安全打开追加目标")
            .map_err(|error| AppendFailure {
                error,
                poison_writer: false,
            })?;
    let original_len = file
        .metadata()
        .await
        .map_err(|source| AppendFailure {
            error: io_error("检查", path, source),
            poison_writer: true,
        })?
        .len();

    let append_error = if let Err(source) = file.write_all(bytes).await {
        Some(io_error("追加", path, source))
    } else if let Err(source) = file.flush().await {
        Some(io_error("刷新", path, source))
    } else if let Err(source) = file.sync_data().await {
        Some(io_error("同步", path, source))
    } else {
        None
    };
    if let Some(error) = append_error {
        return recover_failed_append(&mut file, path, original_len, bytes, error, existed).await;
    }
    drop(file);
    if !existed
        && let Some(parent) = path.parent()
        && let Err(error) = sync_directory(parent).await
    {
        return Err(AppendFailure {
            error,
            poison_writer: true,
        });
    }
    Ok(())
}

async fn recover_failed_append(
    file: &mut fs::File,
    path: &Path,
    original_len: u64,
    expected: &[u8],
    original_error: SessionStoreError,
    existed: bool,
) -> Result<(), AppendFailure> {
    let actual = read_open_file_from_start(file)
        .await
        .map_err(|source| AppendFailure {
        error: SessionStoreError::InvalidStore(format!(
            "追加失败后无法判定 `{}` 的磁盘状态（原错误：{original_error}；回读错误：{source}）。",
            path.display()
        )),
        poison_writer: true,
    })?;
    let original_len_usize = usize::try_from(original_len).map_err(|_| AppendFailure {
        error: SessionStoreError::InvalidStore(format!(
            "追加失败后 `{}` 的原始长度无法在当前平台表示。",
            path.display()
        )),
        poison_writer: true,
    })?;
    if actual.len() == original_len_usize.saturating_add(expected.len())
        && actual.get(original_len_usize..) == Some(expected)
    {
        file.sync_all().await.map_err(|source| AppendFailure {
            error: SessionStoreError::InvalidStore(format!(
                "预期完整行已经落盘，但重新同步 `{}` 仍失败：{source}。",
                path.display()
            )),
            poison_writer: true,
        })?;
        if !existed && let Some(parent) = path.parent() {
            sync_directory(parent)
                .await
                .map_err(|error| AppendFailure {
                    error,
                    poison_writer: true,
                })?;
        }
        return Ok(());
    }

    if actual.len() < original_len_usize {
        return Err(AppendFailure {
            error: SessionStoreError::InvalidStore(format!(
                "追加失败后 `{}` 短于原始长度，无法安全恢复（原错误：{original_error}）。",
                path.display()
            )),
            poison_writer: true,
        });
    }
    file.set_len(original_len)
        .await
        .map_err(|source| AppendFailure {
            error: SessionStoreError::InvalidStore(format!(
                "追加失败后无法截断 `{}`（原错误：{original_error}；恢复错误：{source}）。",
                path.display()
            )),
            poison_writer: true,
        })?;
    file.sync_all().await.map_err(|source| AppendFailure {
        error: SessionStoreError::InvalidStore(format!(
            "追加失败后的截断无法同步 `{}`（原错误：{original_error}；恢复错误：{source}）。",
            path.display()
        )),
        poison_writer: true,
    })?;
    if !existed && let Some(parent) = path.parent() {
        sync_directory(parent)
            .await
            .map_err(|error| AppendFailure {
                error,
                poison_writer: true,
            })?;
    }
    Err(AppendFailure {
        error: original_error,
        poison_writer: false,
    })
}

async fn write_json_lines_synced<T: Serialize>(
    path: &Path,
    values: &[T],
) -> Result<(), SessionStoreError> {
    let mut bytes = Vec::new();
    for value in values {
        serde_json::to_writer(&mut bytes, value).map_err(|source| {
            SessionStoreError::InvalidJson {
                path: path.to_path_buf(),
                source,
            }
        })?;
        bytes.push(b'\n');
    }
    write_bytes_synced(path, &bytes).await
}

async fn write_json_synced<T: Serialize>(path: &Path, value: &T) -> Result<(), SessionStoreError> {
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|source| SessionStoreError::InvalidJson {
            path: path.to_path_buf(),
            source,
        })?;
    write_bytes_synced(path, &bytes).await
}

async fn write_bytes_synced(path: &Path, bytes: &[u8]) -> Result<(), SessionStoreError> {
    let parent = path.parent().ok_or_else(|| {
        SessionStoreError::InvalidStore(format!("原子写入目标 `{}` 缺少父目录。", path.display()))
    })?;
    let metadata = fs::symlink_metadata(parent)
        .await
        .map_err(|source| io_error("检查目录", parent, source))?;
    if metadata_is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(SessionStoreError::InvalidStore(format!(
            "原子写入目标目录 `{}` 不是普通目录或包含符号链接。",
            parent.display()
        )));
    }
    let mut file = open_regular_file_no_follow(path, SecureFileMode::CreateNew, "安全创建")?;
    file.write_all(bytes)
        .await
        .map_err(|source| io_error("写入", path, source))?;
    file.flush()
        .await
        .map_err(|source| io_error("刷新", path, source))?;
    file.sync_all()
        .await
        .map_err(|source| io_error("同步", path, source))?;
    drop(file);
    sync_directory(parent).await?;
    Ok(())
}

async fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, SessionStoreError> {
    let bytes = read_regular_file_no_follow(path).await?;
    serde_json::from_slice(&bytes).map_err(|source| SessionStoreError::InvalidJson {
        path: path.to_path_buf(),
        source,
    })
}

async fn create_dir_all(path: &Path) -> Result<(), SessionStoreError> {
    let existed = path_exists(path).await?;
    fs::create_dir_all(path)
        .await
        .map_err(|source| io_error("创建目录", path, source))?;
    set_private_directory_permissions(path)?;
    if !existed {
        sync_directory(path).await?;
        if let Some(parent) = path.parent() {
            sync_directory(parent).await?;
        }
    }
    Ok(())
}

/// 创建应用所有的存储目录，同时逐级拒绝符号链接和目录逃逸。
async fn ensure_owned_directory(
    storage_root: &Path,
    directory: &Path,
) -> Result<(), SessionStoreError> {
    walk_owned_directory(storage_root, directory, true).await
}

/// 验证已经存在的应用存储目录，不在损坏恢复路径中猜测创建缺失目录。
async fn validate_owned_directory(
    storage_root: &Path,
    directory: &Path,
) -> Result<(), SessionStoreError> {
    walk_owned_directory(storage_root, directory, false).await
}

async fn walk_owned_directory(
    storage_root: &Path,
    directory: &Path,
    create_missing: bool,
) -> Result<(), SessionStoreError> {
    let relative = directory.strip_prefix(storage_root).map_err(|_| {
        SessionStoreError::InvalidStore(format!(
            "会话存储目录 `{}` 逃逸出根目录 `{}`。",
            directory.display(),
            storage_root.display()
        ))
    })?;
    let canonical_root = fs::canonicalize(storage_root)
        .await
        .map_err(|source| io_error("规范化", storage_root, source))?;
    let root_metadata = fs::symlink_metadata(storage_root)
        .await
        .map_err(|source| io_error("检查", storage_root, source))?;
    if metadata_is_link_or_reparse(&root_metadata) || !root_metadata.is_dir() {
        return Err(SessionStoreError::InvalidStore(format!(
            "会话存储根目录 `{}` 不是普通目录。",
            storage_root.display()
        )));
    }

    let mut current = storage_root.to_path_buf();
    let mut expected_current = canonical_root.clone();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(SessionStoreError::InvalidStore(format!(
                "会话存储目录 `{}` 包含不安全路径组件。",
                directory.display()
            )));
        };
        current.push(component);
        expected_current.push(component);
        let metadata = match fs::symlink_metadata(&current).await {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound && create_missing => {
                fs::create_dir(&current)
                    .await
                    .map_err(|source| io_error("创建目录", &current, source))?;
                set_private_directory_permissions(&current)?;
                if let Some(parent) = current.parent() {
                    sync_directory(parent).await?;
                }
                sync_directory(&current).await?;
                fs::symlink_metadata(&current)
                    .await
                    .map_err(|source| io_error("检查", &current, source))?
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(SessionStoreError::InvalidStore(format!(
                    "会话存储目录 `{}` 缺失。",
                    current.display()
                )));
            }
            Err(source) => return Err(io_error("检查", &current, source)),
        };
        if metadata_is_link_or_reparse(&metadata) || !metadata.is_dir() {
            return Err(SessionStoreError::InvalidStore(format!(
                "会话存储目录 `{}` 不是普通目录或包含符号链接。",
                current.display()
            )));
        }
        let canonical_current = fs::canonicalize(&current)
            .await
            .map_err(|source| io_error("规范化", &current, source))?;
        if canonical_current != expected_current {
            return Err(SessionStoreError::InvalidStore(format!(
                "会话存储目录 `{}` 穿过重解析点或逃逸出应用数据根目录。",
                current.display()
            )));
        }
        set_private_directory_permissions(&current)?;
    }
    Ok(())
}

#[cfg(not(windows))]
async fn rename_path(source_path: &Path, destination: &Path) -> Result<(), SessionStoreError> {
    fs::rename(source_path, destination)
        .await
        .map_err(|source| SessionStoreError::Io {
            operation: "原子替换",
            path: destination.to_path_buf(),
            source,
        })?;
    if let Some(parent) = destination.parent() {
        sync_directory(parent).await?;
    }
    if source_path.parent() != destination.parent()
        && let Some(parent) = source_path.parent()
    {
        sync_directory(parent).await?;
    }
    Ok(())
}

#[cfg(windows)]
async fn rename_path(source_path: &Path, destination: &Path) -> Result<(), SessionStoreError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

    let source = source_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: 两个缓冲区均在调用期间有效且以 NUL 结尾；目标 generation
    // 必须不存在，WRITE_THROUGH 要求返回前刷新重命名元数据。
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        return Err(SessionStoreError::Io {
            operation: "原子发布",
            path: destination.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(())
}

async fn sync_generation_staging(staging_dir: &Path) -> Result<(), SessionStoreError> {
    let conversations_dir = staging_dir.join("conversations");
    if path_exists(&conversations_dir).await? {
        sync_directory(&conversations_dir).await?;
    }
    sync_directory(staging_dir).await
}

#[cfg(unix)]
fn open_directory_no_follow(path: &Path) -> Result<std::fs::File, SessionStoreError> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_DIRECTORY);
    let directory = options
        .open(path)
        .map_err(|source| io_error("安全打开目录", path, source))?;
    let metadata = directory
        .metadata()
        .map_err(|source| io_error("检查目录", path, source))?;
    if !metadata.is_dir() {
        return Err(SessionStoreError::InvalidStore(format!(
            "会话存储目录 `{}` 不是普通目录。",
            path.display()
        )));
    }
    Ok(directory)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), SessionStoreError> {
    use std::os::unix::fs::PermissionsExt;

    let directory = open_directory_no_follow(path)?;
    directory
        .set_permissions(std::fs::Permissions::from_mode(0o700))
        .map_err(|source| io_error("收紧目录权限", path, source))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), SessionStoreError> {
    Ok(())
}

#[cfg(unix)]
async fn sync_directory(path: &Path) -> Result<(), SessionStoreError> {
    open_directory_no_follow(path)?
        .sync_all()
        .map_err(|source| io_error("同步目录", path, source))
}

#[cfg(not(unix))]
async fn sync_directory(_path: &Path) -> Result<(), SessionStoreError> {
    Ok(())
}

async fn read_file_entries(directory: &Path) -> Result<Vec<PathBuf>, SessionStoreError> {
    let mut reader = fs::read_dir(directory)
        .await
        .map_err(|source| io_error("读取目录", directory, source))?;
    let mut paths = Vec::new();
    while let Some(entry) = reader
        .next_entry()
        .await
        .map_err(|source| io_error("遍历目录", directory, source))?
    {
        let path = entry.path();
        let metadata = entry
            .file_type()
            .await
            .map_err(|source| io_error("检查", &path, source))?;
        if metadata.is_symlink() || !metadata.is_file() {
            return Err(SessionStoreError::InvalidStore(format!(
                "会话存储目录包含非普通文件 `{}`。",
                path.display()
            )));
        }
        paths.push(path);
    }
    Ok(paths)
}

async fn path_exists(path: &Path) -> Result<bool, SessionStoreError> {
    match fs::try_exists(path).await {
        Ok(exists) => Ok(exists),
        Err(source) => Err(io_error("检查", path, source)),
    }
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> SessionStoreError {
    SessionStoreError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

fn validate_non_empty(field: &'static str, value: &str) -> Result<(), SessionStoreError> {
    if value.trim().is_empty() {
        return Err(SessionStoreError::InvalidInput(format!(
            "会话事件字段 `{field}` 不能为空。"
        )));
    }
    Ok(())
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn hash_text(value: &str) -> String {
    hash_bytes(value.as_bytes())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hash_bytes(value: &[u8]) -> String {
    format_hash(Sha256::digest(value).as_slice())
}

fn format_hash(value: &[u8]) -> String {
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value {
        use std::fmt::Write;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn encode_hex(value: &[u8]) -> String {
    format_hash(value)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    #[cfg(unix)]
    use std::path::Path;

    use serde_json::json;
    use tempfile::tempdir;

    use super::{SessionStore, conversation_file_name};

    async fn migration_combines_legacy_sources_with_backup_dedup_and_quarantine() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(sessions.join("conversations"))
            .await
            .unwrap();
        let user = r#"{"kind":"user","time":"2026-07-11T00:00:00Z","payload":{"conversation_id":"chat/a","turn_id":"turn-1","content":"hello"}}"#;
        let assistant = r#"{"kind":"assistant","time":"2026-07-11T00:00:01Z","payload":{"conversation_id":"chat/a","turn_id":"turn-1","content":"hi"}}"#;
        let global = format!("{user}\nnot-json\n{assistant}\n");
        let per_conversation = format!(
            "{user}\n{assistant}\n{}\n",
            r#"{"kind":"user","time":"2026-07-11T00:00:02Z","payload":{"conversation_id":"chat/a","turn_id":"turn-2","content":"again"}}"#
        );
        tokio::fs::write(sessions.join("runtime.jsonl"), global.as_bytes())
            .await
            .unwrap();
        tokio::fs::write(
            sessions.join("conversations").join("chat-a.jsonl"),
            per_conversation.as_bytes(),
        )
        .await
        .unwrap();

        let (store, report) = SessionStore::open(temp.path()).await.unwrap();

        assert_eq!(report.legacy_sources, 2);
        assert_eq!(report.source_records, 6);
        assert_eq!(report.migrated_records, 3);
        assert_eq!(report.deduplicated_records, 2);
        assert_eq!(report.quarantined_records, 1);
        assert!(!report.reused_existing_store);
        let events = store.events_for_conversation("chat/a").await.unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(
            events
                .iter()
                .map(|event| event.commit_seq)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(events.iter().all(|event| event.legacy_record.is_some()));

        let backup = report.backup_dir.unwrap();
        assert_eq!(
            tokio::fs::read(backup.join("runtime.jsonl")).await.unwrap(),
            global.as_bytes()
        );
        assert_eq!(
            tokio::fs::read(backup.join("conversations/chat-a.jsonl"))
                .await
                .unwrap(),
            per_conversation.as_bytes()
        );
        assert!(
            sessions
                .join("generations")
                .join(store.generation_id())
                .join("quarantine.jsonl")
                .exists()
        );
        assert!(sessions.join("store.json").exists());
    }

    async fn migration_cleans_a_partial_backup_temp_before_atomic_publication() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(&sessions).await.unwrap();
        let legacy = b"{\"kind\":\"user\",\"payload\":{\"conversation_id\":\"chat\"}}\n";
        tokio::fs::write(sessions.join("runtime.jsonl"), legacy)
            .await
            .unwrap();
        let sources = super::discover_legacy_sources(&sessions).await.unwrap();
        let migration_id = super::migration_id(&sources);
        let destination = sessions
            .join("backups")
            .join(migration_id)
            .join("runtime.jsonl");
        tokio::fs::create_dir_all(destination.parent().unwrap())
            .await
            .unwrap();
        let stale = destination.parent().unwrap().join(format!(
            "{}crashed.tmp",
            super::backup_temporary_prefix(&destination)
        ));
        tokio::fs::write(&stale, b"partial-backup").await.unwrap();

        let (_, report) = SessionStore::open(temp.path()).await.unwrap();

        assert_eq!(tokio::fs::read(&destination).await.unwrap(), legacy);
        assert!(!stale.exists());
        assert_eq!(
            report.backup_dir.unwrap(),
            tokio::fs::canonicalize(destination.parent().unwrap())
                .await
                .unwrap()
        );
    }

    async fn atomic_backup_publication_is_idempotent_for_verified_existing_bytes() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(&sessions).await.unwrap();
        let legacy = b"{\"kind\":\"assistant\",\"payload\":{\"conversation_id\":\"chat\"}}\n";
        tokio::fs::write(sessions.join("runtime.jsonl"), legacy)
            .await
            .unwrap();
        let sources = super::discover_legacy_sources(&sessions).await.unwrap();
        let backup_dir = sessions.join("verified-backup");

        super::backup_sources(&sources, &sessions, &backup_dir)
            .await
            .unwrap();
        super::backup_sources(&sources, &sessions, &backup_dir)
            .await
            .unwrap();

        assert_eq!(
            tokio::fs::read(backup_dir.join("runtime.jsonl"))
                .await
                .unwrap(),
            legacy
        );
    }

    async fn migration_stably_inserts_a_conversation_only_record_between_mirror_anchors() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(sessions.join("conversations"))
            .await
            .unwrap();
        let record = |content: &str| {
            format!(
                r#"{{"kind":"assistant","payload":{{"conversation_id":"chat","content":"{content}"}}}}"#
            )
        };
        let a = record("A");
        let b = record("B");
        let c = record("C");
        tokio::fs::write(sessions.join("runtime.jsonl"), format!("{a}\n{c}\n"))
            .await
            .unwrap();
        tokio::fs::write(
            sessions.join("conversations/chat.jsonl"),
            format!("{a}\n{b}\n{c}\n"),
        )
        .await
        .unwrap();

        let (store, report) = SessionStore::open(temp.path()).await.unwrap();
        let contents = store
            .events_for_conversation("chat")
            .await
            .unwrap()
            .into_iter()
            .map(|event| event.payload["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();

        assert_eq!(contents, ["A", "B", "C"]);
        assert_eq!(report.migrated_records, 3);
        assert_eq!(report.deduplicated_records, 2);
    }

    async fn migration_aligns_legal_duplicates_by_source_order() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(sessions.join("conversations"))
            .await
            .unwrap();
        let record = |content: &str| {
            format!(
                r#"{{"kind":"assistant","payload":{{"conversation_id":"chat","content":"{content}"}}}}"#
            )
        };
        let a = record("A");
        let b = record("B");
        let c = record("C");
        tokio::fs::write(sessions.join("runtime.jsonl"), format!("{a}\n{a}\n{c}\n"))
            .await
            .unwrap();
        tokio::fs::write(
            sessions.join("conversations/chat.jsonl"),
            format!("{a}\n{b}\n{a}\n{c}\n"),
        )
        .await
        .unwrap();

        let (store, report) = SessionStore::open(temp.path()).await.unwrap();
        let contents = store
            .events_for_conversation("chat")
            .await
            .unwrap()
            .into_iter()
            .map(|event| event.payload["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();

        assert_eq!(contents, ["A", "B", "A", "C"]);
        assert_eq!(report.migrated_records, 4);
        assert_eq!(report.deduplicated_records, 3);
    }

    async fn opening_an_existing_store_is_idempotent() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(&sessions).await.unwrap();
        tokio::fs::write(
            sessions.join("runtime.jsonl"),
            b"{\"kind\":\"user\",\"payload\":{\"conversation_id\":\"chat\"}}\n",
        )
        .await
        .unwrap();

        let (first, first_report) = SessionStore::open(temp.path()).await.unwrap();
        let (second, second_report) = SessionStore::open(temp.path()).await.unwrap();

        assert_eq!(first.generation_id(), second.generation_id());
        assert_eq!(first_report.migration_id, second_report.migration_id);
        assert!(second_report.reused_existing_store);
        assert_eq!(second.aggregate_events().await.unwrap().len(), 1);
    }

    async fn conversation_only_legacy_source_migrates_without_a_global_transcript() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(sessions.join("conversations"))
            .await
            .unwrap();
        tokio::fs::write(
            sessions.join("conversations/only-chat.jsonl"),
            b"{\"kind\":\"user\",\"payload\":{\"conversation_id\":\"only-chat\",\"content\":\"hello\"}}\n",
        )
        .await
        .unwrap();

        let (store, report) = SessionStore::open(temp.path()).await.unwrap();

        assert_eq!(report.legacy_sources, 1);
        assert_eq!(report.source_records, 1);
        assert_eq!(report.migrated_records, 1);
        assert_eq!(
            store.events_for_conversation("only-chat").await.unwrap()[0].payload["content"],
            "hello"
        );
    }

    async fn opening_rejects_a_modified_read_only_backup() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(&sessions).await.unwrap();
        let legacy = b"{\"kind\":\"user\",\"payload\":{\"conversation_id\":\"chat\"}}\n";
        tokio::fs::write(sessions.join("runtime.jsonl"), legacy)
            .await
            .unwrap();

        let (store, report) = SessionStore::open(temp.path()).await.unwrap();
        drop(store);
        let backup = report.backup_dir.unwrap().join("runtime.jsonl");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        tokio::fs::write(&backup, b"tampered").await.unwrap();

        let error = SessionStore::open(temp.path()).await.unwrap_err();
        assert!(error.to_string().contains("SHA-256 不一致"));
        assert_eq!(
            tokio::fs::read(sessions.join("runtime.jsonl"))
                .await
                .unwrap(),
            legacy
        );
    }

    async fn incremental_migration_rejects_a_truncated_previous_backup() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(&sessions).await.unwrap();
        let first = b"{\"kind\":\"user\",\"payload\":{\"conversation_id\":\"chat\"}}\n";
        tokio::fs::write(sessions.join("runtime.jsonl"), first)
            .await
            .unwrap();

        let (store, report) = SessionStore::open(temp.path()).await.unwrap();
        drop(store);
        let backup = report.backup_dir.unwrap().join("runtime.jsonl");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        tokio::fs::write(backup, b"").await.unwrap();
        let mut appended = first.to_vec();
        appended.extend_from_slice(
            b"{\"kind\":\"assistant\",\"payload\":{\"conversation_id\":\"chat\"}}\n",
        );
        tokio::fs::write(sessions.join("runtime.jsonl"), appended)
            .await
            .unwrap();

        let error = SessionStore::open(temp.path()).await.unwrap_err();
        assert!(error.to_string().contains("SHA-256 与迁移清单不一致"));
    }

    #[cfg(unix)]
    async fn migration_rejects_a_symlinked_legacy_conversations_directory() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(&sessions).await.unwrap();
        tokio::fs::write(
            outside.path().join("chat.jsonl"),
            b"{\"kind\":\"user\",\"payload\":{\"conversation_id\":\"chat\"}}\n",
        )
        .await
        .unwrap();
        symlink(outside.path(), sessions.join("conversations")).unwrap();

        let error = SessionStore::open(temp.path()).await.unwrap_err();
        assert!(error.to_string().contains("符号链接"));
    }

    #[cfg(unix)]
    async fn runtime_operations_fail_closed_after_a_conversation_file_becomes_a_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(&sessions).await.unwrap();
        tokio::fs::write(
            sessions.join("runtime.jsonl"),
            b"{\"kind\":\"user\",\"payload\":{\"conversation_id\":\"chat\"}}\n",
        )
        .await
        .unwrap();

        let (store, _) = SessionStore::open(temp.path()).await.unwrap();
        let conversation_path = sessions
            .join("generations")
            .join(store.generation_id())
            .join("conversations")
            .join(conversation_file_name("chat"));
        let outside_file = outside.path().join("outside.jsonl");
        tokio::fs::write(&outside_file, b"outside-must-not-change\n")
            .await
            .unwrap();
        tokio::fs::remove_file(&conversation_path).await.unwrap();
        symlink(&outside_file, &conversation_path).unwrap();

        let append_error = store
            .append_event(
                "chat",
                Some("turn-attack".to_string()),
                "assistant",
                json!({"content":"must-not-escape"}),
            )
            .await
            .unwrap_err();
        assert!(append_error.to_string().contains("安全打开追加目标"));
        assert!(store.events_for_conversation("chat").await.is_err());
        assert_eq!(
            tokio::fs::read(&outside_file).await.unwrap(),
            b"outside-must-not-change\n"
        );

        drop(store);
        assert!(SessionStore::open(temp.path()).await.is_err());
        assert_eq!(
            tokio::fs::read(&outside_file).await.unwrap(),
            b"outside-must-not-change\n"
        );
    }

    #[cfg(unix)]
    async fn atomic_new_file_write_never_follows_a_precreated_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let outside = temp.path().join("outside.json");
        let target = temp.path().join("pointer.tmp");
        tokio::fs::write(&outside, b"outside").await.unwrap();
        symlink(&outside, &target).unwrap();

        assert!(
            super::write_bytes_synced(&target, b"replacement")
                .await
                .is_err()
        );
        assert_eq!(tokio::fs::read(outside).await.unwrap(), b"outside");
    }

    #[cfg(unix)]
    async fn session_generations_and_backups_use_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(&sessions).await.unwrap();
        tokio::fs::write(
            sessions.join("runtime.jsonl"),
            b"{\"kind\":\"user\",\"payload\":{\"conversation_id\":\"chat\"}}\n",
        )
        .await
        .unwrap();

        let (store, report) = SessionStore::open(temp.path()).await.unwrap();
        let generation = sessions.join("generations").join(store.generation_id());
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;

        for directory in [
            sessions.clone(),
            sessions.join("generations"),
            generation.clone(),
            generation.join("conversations"),
            report.backup_dir.clone().unwrap(),
        ] {
            assert_eq!(mode(&directory), 0o700, "{} 权限过宽", directory.display());
        }
        for file in [
            sessions.join("store.json"),
            generation.join("manifest.json"),
            generation.join("runtime-manifest.json"),
            generation
                .join("conversations")
                .join(conversation_file_name("chat")),
        ] {
            assert_eq!(mode(&file), 0o600, "{} 权限过宽", file.display());
        }
        assert_eq!(
            mode(&report.backup_dir.unwrap().join("runtime.jsonl")),
            0o400
        );
    }

    async fn appended_legacy_records_create_an_incremental_generation() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(&sessions).await.unwrap();
        let first_record =
            b"{\"kind\":\"user\",\"payload\":{\"conversation_id\":\"chat\",\"content\":\"first\"}}\n";
        tokio::fs::write(sessions.join("runtime.jsonl"), first_record)
            .await
            .unwrap();

        let (first_store, _) = SessionStore::open(temp.path()).await.unwrap();
        first_store
            .append_event(
                "chat",
                Some("turn-v3".to_string()),
                "assistant",
                json!({"content":"native"}),
            )
            .await
            .unwrap();
        let previous_generation = first_store.generation_id().to_string();

        let second_record =
            b"{\"kind\":\"assistant\",\"payload\":{\"conversation_id\":\"chat\",\"content\":\"legacy-delta\"}}\n";
        let mut appended = first_record.to_vec();
        appended.extend_from_slice(second_record);
        tokio::fs::write(sessions.join("runtime.jsonl"), &appended)
            .await
            .unwrap();

        let (second_store, report) = SessionStore::open(temp.path()).await.unwrap();
        assert_ne!(second_store.generation_id(), previous_generation);
        assert!(!report.reused_existing_store);
        let events = second_store.events_for_conversation("chat").await.unwrap();
        assert_eq!(events.len(), 3);
        assert!(
            events
                .iter()
                .any(|event| event.payload["content"] == "native")
        );
        assert!(
            events
                .iter()
                .any(|event| event.payload["content"] == "legacy-delta")
        );

        let (third_store, third_report) = SessionStore::open(temp.path()).await.unwrap();
        assert_eq!(third_store.generation_id(), second_store.generation_id());
        assert!(third_report.reused_existing_store);
        assert_eq!(third_store.aggregate_events().await.unwrap().len(), 3);
    }

    async fn incremental_migration_rebuilds_cross_generation_mirror_order() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(sessions.join("conversations"))
            .await
            .unwrap();
        let record = |content: &str| {
            format!(
                r#"{{"kind":"assistant","payload":{{"conversation_id":"chat","content":"{content}"}}}}"#
            )
        };
        let a = record("A");
        let b = record("B");
        let c = record("C");
        tokio::fs::write(sessions.join("runtime.jsonl"), format!("{a}\n{c}\n"))
            .await
            .unwrap();
        let conversation_path = sessions.join("conversations/chat.jsonl");
        tokio::fs::write(&conversation_path, format!("{a}\n"))
            .await
            .unwrap();

        let (first_store, _) = SessionStore::open(temp.path()).await.unwrap();
        let first_contents = first_store
            .events_for_conversation("chat")
            .await
            .unwrap()
            .into_iter()
            .map(|event| event.payload["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(first_contents, ["A", "C"]);
        first_store
            .append_event(
                "chat",
                Some("turn-native".to_string()),
                "assistant",
                json!({"content": "NATIVE"}),
            )
            .await
            .unwrap();

        tokio::fs::write(&conversation_path, format!("{a}\n{b}\n{c}\n"))
            .await
            .unwrap();
        let (second_store, report) = SessionStore::open(temp.path()).await.unwrap();
        let second_contents = second_store
            .events_for_conversation("chat")
            .await
            .unwrap()
            .into_iter()
            .map(|event| event.payload["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();

        assert_eq!(second_contents, ["A", "B", "C", "NATIVE"]);
        assert_eq!(report.source_records, 5);
        assert_eq!(report.migrated_records, 3);
        assert_eq!(report.deduplicated_records, 2);

        let (third_store, third_report) = SessionStore::open(temp.path()).await.unwrap();
        assert_eq!(third_store.generation_id(), second_store.generation_id());
        assert!(third_report.reused_existing_store);
    }

    async fn incremental_migration_keeps_trailing_unique_legacy_after_native_events() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(sessions.join("conversations"))
            .await
            .unwrap();
        let record = |content: &str| {
            format!(
                r#"{{"kind":"assistant","payload":{{"conversation_id":"chat","content":"{content}"}}}}"#
            )
        };
        let a = record("A");
        let b = record("B");
        tokio::fs::write(sessions.join("runtime.jsonl"), format!("{a}\n"))
            .await
            .unwrap();
        let conversation_path = sessions.join("conversations/chat.jsonl");
        tokio::fs::write(&conversation_path, format!("{a}\n"))
            .await
            .unwrap();

        let (first_store, _) = SessionStore::open(temp.path()).await.unwrap();
        first_store
            .append_event(
                "chat",
                Some("turn-native".to_string()),
                "assistant",
                json!({"content": "NATIVE"}),
            )
            .await
            .unwrap();

        tokio::fs::write(&conversation_path, format!("{a}\n{b}\n"))
            .await
            .unwrap();
        let (second_store, _) = SessionStore::open(temp.path()).await.unwrap();
        let contents = second_store
            .events_for_conversation("chat")
            .await
            .unwrap()
            .into_iter()
            .map(|event| event.payload["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();

        assert_eq!(contents, ["A", "NATIVE", "B"]);
    }

    async fn a_truncated_tail_is_reparsed_after_legacy_completes_the_line() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(&sessions).await.unwrap();
        let truncated =
            b"{\"kind\":\"user\",\"payload\":{\"conversation_id\":\"chat\",\"content\":\"hel";
        tokio::fs::write(sessions.join("runtime.jsonl"), truncated)
            .await
            .unwrap();

        let (first, first_report) = SessionStore::open(temp.path()).await.unwrap();
        assert!(first.aggregate_events().await.unwrap().is_empty());
        assert_eq!(first_report.quarantined_records, 1);

        let mut completed = truncated.to_vec();
        completed.extend_from_slice(b"lo\"}}\n");
        tokio::fs::write(sessions.join("runtime.jsonl"), completed)
            .await
            .unwrap();
        let (second, second_report) = SessionStore::open(temp.path()).await.unwrap();
        let events = second.events_for_conversation("chat").await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["content"], "hello");
        assert_eq!(second_report.quarantined_records, 0);
    }

    async fn mirrors_appended_in_separate_starts_preserve_legal_multiplicity() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(sessions.join("conversations"))
            .await
            .unwrap();
        let record =
            b"{\"kind\":\"user\",\"payload\":{\"conversation_id\":\"chat\",\"content\":\"same\"}}\n";
        tokio::fs::write(sessions.join("runtime.jsonl"), record)
            .await
            .unwrap();
        let (first, _) = SessionStore::open(temp.path()).await.unwrap();
        assert_eq!(first.aggregate_events().await.unwrap().len(), 1);

        let conversation_path = sessions.join("conversations/chat.jsonl");
        tokio::fs::write(&conversation_path, record).await.unwrap();
        let (second, second_report) = SessionStore::open(temp.path()).await.unwrap();
        assert_eq!(second.aggregate_events().await.unwrap().len(), 1);
        assert_eq!(second_report.deduplicated_records, 1);

        let mut repeated = record.to_vec();
        repeated.extend_from_slice(record);
        tokio::fs::write(&conversation_path, repeated)
            .await
            .unwrap();
        let (third, _) = SessionStore::open(temp.path()).await.unwrap();
        assert_eq!(third.aggregate_events().await.unwrap().len(), 2);
    }

    async fn migration_rejects_tool_result_without_a_preceding_call_before_publication() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(&sessions).await.unwrap();
        tokio::fs::write(
            sessions.join("runtime.jsonl"),
            br#"{"kind":"tool_result","payload":{"conversation_id":"chat","turn_id":"turn-1","call_id":"call-1","content":"unexpected"}}
"#,
        )
        .await
        .unwrap();

        let error = SessionStore::open(temp.path()).await.unwrap_err();

        assert!(
            error
                .to_string()
                .contains("缺少同回合 `turn-1` 的前置 tool_call")
        );
        assert!(!sessions.join("store.json").exists());
        let mut generations = tokio::fs::read_dir(sessions.join("generations"))
            .await
            .unwrap();
        while let Some(entry) = generations.next_entry().await.unwrap() {
            assert!(entry.file_name().to_string_lossy().starts_with(".staging-"));
        }
    }

    async fn migration_publishes_a_valid_tool_call_and_result_pair() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(&sessions).await.unwrap();
        let transcript = concat!(
            r#"{"kind":"tool_call","payload":{"conversation_id":"chat","turn_id":"turn-1","call_id":"call-1","tool":"file_read"}}"#,
            "\n",
            r#"{"kind":"tool_result","payload":{"conversation_id":"chat","turn_id":"turn-1","call_id":"call-1","tool":"file_read","success":true}}"#,
            "\n"
        );
        tokio::fs::write(sessions.join("runtime.jsonl"), transcript)
            .await
            .unwrap();

        let (store, report) = SessionStore::open(temp.path()).await.unwrap();

        let events = store.events_for_conversation("chat").await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "tool_call");
        assert_eq!(events[1].kind, "tool_result");
        assert_eq!(report.migrated_records, 2);
        assert!(sessions.join("store.json").exists());
    }

    async fn cloned_stores_share_one_serial_writer() {
        let temp = tempdir().unwrap();
        let (store, _) = SessionStore::open(temp.path()).await.unwrap();
        let mut tasks = Vec::new();
        for index in 0..32usize {
            let store = store.clone();
            tasks.push(tokio::spawn(async move {
                store
                    .append_event(
                        "chat",
                        Some(format!("turn-{index}")),
                        "user",
                        json!({"index": index}),
                    )
                    .await
                    .unwrap()
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }

        let events = store.events_for_conversation("chat").await.unwrap();
        assert_eq!(events.len(), 32);
        let sequences = events
            .iter()
            .map(|event| event.commit_seq)
            .collect::<BTreeSet<_>>();
        assert_eq!(sequences.len(), 32);
        assert_eq!(sequences.first().copied(), Some(1));
        assert_eq!(sequences.last().copied(), Some(32));
        assert_eq!(store.aggregate_events().await.unwrap().len(), 32);
    }

    async fn separately_opened_handles_still_share_the_serial_writer() {
        let temp = tempdir().unwrap();
        let (first, _) = SessionStore::open(temp.path()).await.unwrap();
        let (second, _) = SessionStore::open(temp.path()).await.unwrap();
        let first_task = tokio::spawn(async move {
            first
                .append_event("chat", None, "user", json!({"source": "first"}))
                .await
                .unwrap()
        });
        let second_task = tokio::spawn(async move {
            second
                .append_event("chat", None, "user", json!({"source": "second"}))
                .await
                .unwrap()
        });

        let first_event = first_task.await.unwrap();
        let second_event = second_task.await.unwrap();
        assert_ne!(first_event.commit_seq, second_event.commit_seq);
        assert_eq!(
            BTreeSet::from([first_event.commit_seq, second_event.commit_seq]),
            BTreeSet::from([1, 2])
        );
    }

    async fn opening_repairs_a_complete_event_that_only_lacks_the_final_newline() {
        let temp = tempdir().unwrap();
        let (store, _) = SessionStore::open(temp.path()).await.unwrap();
        let event = store
            .append_event(
                "chat",
                Some("turn-1".to_string()),
                "user",
                json!({"content":"hi"}),
            )
            .await
            .unwrap();
        let path = store.conversation_path("chat");
        let mut bytes = tokio::fs::read(&path).await.unwrap();
        assert_eq!(bytes.pop(), Some(b'\n'));
        tokio::fs::write(&path, &bytes).await.unwrap();
        drop(store);

        let (reopened, _) = SessionStore::open(temp.path()).await.unwrap();
        assert_eq!(
            reopened.events_for_conversation("chat").await.unwrap(),
            [event]
        );
        assert!(tokio::fs::read(path).await.unwrap().ends_with(b"\n"));
    }

    async fn opening_quarantines_and_truncates_an_incomplete_runtime_tail() {
        let temp = tempdir().unwrap();
        let (store, _) = SessionStore::open(temp.path()).await.unwrap();
        let committed = store
            .append_event(
                "chat",
                Some("turn-1".to_string()),
                "user",
                json!({"content":"ok"}),
            )
            .await
            .unwrap();
        let path = store.conversation_path("chat");
        let mut bytes = tokio::fs::read(&path).await.unwrap();
        bytes.extend_from_slice(br#"{"schema_version":"muse-session-event/v3","event_id":"half"#);
        tokio::fs::write(&path, bytes).await.unwrap();
        let generation_dir = store.inner.generation_dir.clone();
        drop(store);

        let (reopened, _) = SessionStore::open(temp.path()).await.unwrap();
        assert_eq!(
            reopened.events_for_conversation("chat").await.unwrap(),
            [committed]
        );
        let quarantine = tokio::fs::read_to_string(generation_dir.join("runtime-quarantine.jsonl"))
            .await
            .unwrap();
        assert!(quarantine.contains("活动 transcript 末尾是不完整或非法"));
        assert!(quarantine.contains("68616c66"));
    }

    async fn opening_still_rejects_a_complete_bad_runtime_line() {
        let temp = tempdir().unwrap();
        let (store, _) = SessionStore::open(temp.path()).await.unwrap();
        store
            .append_event("chat", None, "user", json!({"content":"ok"}))
            .await
            .unwrap();
        let path = store.conversation_path("chat");
        let mut bytes = tokio::fs::read(&path).await.unwrap();
        bytes.extend_from_slice(b"not-json\n");
        tokio::fs::write(&path, bytes).await.unwrap();
        drop(store);

        let error = SessionStore::open(temp.path()).await.unwrap_err();
        assert!(error.to_string().contains("不是有效 JSON"));
    }

    async fn a_persisted_reservation_is_not_reused_after_restart() {
        let temp = tempdir().unwrap();
        let (store, _) = SessionStore::open(temp.path()).await.unwrap();
        {
            let mut writer = store.inner.writer.lock().await;
            let reserved = writer.next_commit_seq;
            writer.next_commit_seq = reserved + 1;
            let mut manifest = writer.runtime_manifest.clone();
            manifest.last_commit_seq = reserved;
            manifest.updated_at = chrono::Utc::now().to_rfc3339();
            super::persist_runtime_manifest(&store.inner.generation_dir, &manifest)
                .await
                .unwrap();
            writer.runtime_manifest = manifest;
        }
        drop(store);

        let (reopened, _) = SessionStore::open(temp.path()).await.unwrap();
        let event = reopened
            .append_event("chat", None, "user", json!({"content":"after-crash"}))
            .await
            .unwrap();
        assert_eq!(event.commit_seq, 2);
        super::validate_generation_for_publication(
            &reopened.inner.generation_dir,
            reopened.generation_id(),
        )
        .await
        .unwrap();
    }

    async fn incremental_migration_never_reuses_a_persisted_high_watermark() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(&sessions).await.unwrap();
        let first =
            b"{\"kind\":\"user\",\"payload\":{\"conversation_id\":\"chat\",\"content\":\"first\"}}\n";
        tokio::fs::write(sessions.join("runtime.jsonl"), first)
            .await
            .unwrap();

        let (store, _) = SessionStore::open(temp.path()).await.unwrap();
        let generation_dir = sessions.join("generations").join(store.generation_id());
        drop(store);

        // 模拟一次已经持久化序号保留、但事件行尚未完成便崩溃的状态。
        let mut runtime_manifest = super::load_runtime_manifest(&generation_dir).await.unwrap();
        runtime_manifest.last_commit_seq = 50;
        super::persist_runtime_manifest(&generation_dir, &runtime_manifest)
            .await
            .unwrap();

        let second =
            b"{\"kind\":\"assistant\",\"payload\":{\"conversation_id\":\"chat\",\"content\":\"second\"}}\n";
        let mut appended = first.to_vec();
        appended.extend_from_slice(second);
        tokio::fs::write(sessions.join("runtime.jsonl"), appended)
            .await
            .unwrap();

        let (migrated, _) = SessionStore::open(temp.path()).await.unwrap();
        let events = migrated.events_for_conversation("chat").await.unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.commit_seq)
                .collect::<Vec<_>>(),
            vec![51, 52]
        );
        let next = migrated
            .append_event(
                "chat",
                Some("turn-next".to_string()),
                "assistant",
                json!({}),
            )
            .await
            .unwrap();
        assert_eq!(next.commit_seq, 53);
    }

    async fn recovery_rejects_an_untrusted_pending_delete_path_before_touching_disk() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(&sessions).await.unwrap();
        tokio::fs::write(
            sessions.join("runtime.jsonl"),
            b"{\"kind\":\"user\",\"payload\":{\"conversation_id\":\"chat\"}}\n",
        )
        .await
        .unwrap();

        let (store, _) = SessionStore::open(temp.path()).await.unwrap();
        let generation_dir = sessions.join("generations").join(store.generation_id());
        drop(store);
        let victim = sessions.join("victim.txt");
        tokio::fs::write(&victim, b"must-survive").await.unwrap();

        let mut runtime_manifest = super::load_runtime_manifest(&generation_dir).await.unwrap();
        runtime_manifest
            .pending_deletions
            .insert("malicious".to_string(), "../../../victim.txt".to_string());
        super::persist_runtime_manifest(&generation_dir, &runtime_manifest)
            .await
            .unwrap();

        let error = SessionStore::open(temp.path()).await.unwrap_err();
        assert!(error.to_string().contains("SHA-256"));
        assert_eq!(tokio::fs::read(victim).await.unwrap(), b"must-survive");
    }

    async fn opening_rejects_an_unsafe_migration_id_before_resolving_backup_paths() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(&sessions).await.unwrap();
        tokio::fs::write(
            sessions.join("runtime.jsonl"),
            b"{\"kind\":\"user\",\"payload\":{\"conversation_id\":\"chat\"}}\n",
        )
        .await
        .unwrap();

        let (store, _) = SessionStore::open(temp.path()).await.unwrap();
        let generation_dir = sessions.join("generations").join(store.generation_id());
        drop(store);
        let manifest_path = generation_dir.join("manifest.json");
        let mut manifest: super::GenerationManifest =
            super::read_json(&manifest_path).await.unwrap();
        manifest.migration_id = "../../outside".to_string();
        let tampered_path = generation_dir.join(".tampered-manifest.tmp");
        super::write_json_synced(&tampered_path, &manifest)
            .await
            .unwrap();
        super::replace_file_path(&tampered_path, &manifest_path)
            .await
            .unwrap();

        let error = SessionStore::open(temp.path()).await.unwrap_err();
        assert!(error.to_string().contains("迁移标识不是有效 SHA-256"));
    }

    async fn opening_recovers_a_published_generation_when_pointer_was_not_written() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        tokio::fs::create_dir_all(&sessions).await.unwrap();
        tokio::fs::write(
            sessions.join("runtime.jsonl"),
            b"{\"kind\":\"user\",\"payload\":{\"conversation_id\":\"chat\"}}\n",
        )
        .await
        .unwrap();

        let (first, _) = SessionStore::open(temp.path()).await.unwrap();
        let generation_id = first.generation_id().to_string();
        drop(first);
        tokio::fs::remove_file(sessions.join("store.json"))
            .await
            .unwrap();

        let (recovered, report) = SessionStore::open(temp.path()).await.unwrap();
        assert_eq!(recovered.generation_id(), generation_id);
        assert!(report.reused_existing_store);
        assert!(sessions.join("store.json").is_file());
    }

    async fn pending_delete_failure_poisons_writer_until_restart_completes_recovery() {
        let temp = tempdir().unwrap();
        let (store, _) = SessionStore::open(temp.path()).await.unwrap();
        store
            .append_event("chat", None, "user", json!({"content":"delete-me"}))
            .await
            .unwrap();

        let error = store
            .delete_conversation_with_completion("chat", |_| async {
                Err(super::SessionStoreError::InvalidStore(
                    "模拟 remove_file 失败".to_string(),
                ))
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("模拟 remove_file 失败"));
        let append_error = store
            .append_event("other", None, "user", json!({"content":"blocked"}))
            .await
            .unwrap_err();
        assert!(append_error.to_string().contains("写者已进入保护状态"));
        drop(store);

        let (reopened, _) = SessionStore::open(temp.path()).await.unwrap();
        assert!(
            reopened
                .events_for_conversation("chat")
                .await
                .unwrap()
                .is_empty()
        );
        let event = reopened
            .append_event("other", None, "user", json!({"content":"after-recovery"}))
            .await
            .unwrap();
        assert_eq!(event.commit_seq, 2);
    }

    async fn failed_append_recovery_truncates_a_partial_line_without_poisoning() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("events.jsonl");
        let original = b"existing\n";
        let expected = b"complete-event\n";
        let mut partial = original.to_vec();
        partial.extend_from_slice(&expected[..5]);
        tokio::fs::write(&path, partial).await.unwrap();
        let mut file = super::open_regular_file_no_follow(
            &path,
            super::SecureFileMode::ReadWrite,
            "打开测试事件",
        )
        .unwrap();

        let failure = super::recover_failed_append(
            &mut file,
            &path,
            original.len() as u64,
            expected,
            super::SessionStoreError::InvalidStore("模拟同步失败".to_string()),
            true,
        )
        .await
        .unwrap_err();

        assert!(!failure.poison_writer);
        assert_eq!(tokio::fs::read(path).await.unwrap(), original);
    }

    async fn failed_append_recovery_accepts_an_exact_complete_line_after_resync() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("events.jsonl");
        let original = b"existing\n";
        let expected = b"complete-event\n";
        let mut complete = original.to_vec();
        complete.extend_from_slice(expected);
        tokio::fs::write(&path, &complete).await.unwrap();
        let mut file = super::open_regular_file_no_follow(
            &path,
            super::SecureFileMode::ReadWrite,
            "打开测试事件",
        )
        .unwrap();

        super::recover_failed_append(
            &mut file,
            &path,
            original.len() as u64,
            expected,
            super::SessionStoreError::InvalidStore("模拟同步失败".to_string()),
            true,
        )
        .await
        .unwrap();

        assert_eq!(tokio::fs::read(path).await.unwrap(), complete);
    }

    async fn failed_append_recovery_poisons_when_original_bytes_are_missing() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("events.jsonl");
        tokio::fs::write(&path, b"short").await.unwrap();
        let mut file = super::open_regular_file_no_follow(
            &path,
            super::SecureFileMode::ReadWrite,
            "打开测试事件",
        )
        .unwrap();

        let failure = super::recover_failed_append(
            &mut file,
            &path,
            20,
            b"expected\n",
            super::SessionStoreError::InvalidStore("模拟同步失败".to_string()),
            true,
        )
        .await
        .unwrap_err();

        assert!(failure.poison_writer);
    }

    fn conversation_file_names_are_fixed_hashes() {
        let unsafe_name = conversation_file_name("../../outside");
        assert_eq!(unsafe_name.len(), 64 + ".jsonl".len());
        assert!(!unsafe_name.contains('/'));
        assert_ne!(unsafe_name, conversation_file_name("../outside"));
        assert_eq!(unsafe_name, conversation_file_name("../../outside"));
    }
    #[test]
    fn aggregated_sync_test_cases() {
        let mut failures = Vec::new();
        {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                conversation_file_names_are_fixed_hashes()
            }))
            .is_err()
            {
                failures.push("conversation_file_names_are_fixed_hashes");
            }
        }
        assert!(failures.is_empty(), "聚合测试失败：{}", failures.join(", "));
    }

    #[tokio::test]
    async fn aggregated_async_test_cases() {
        use futures::FutureExt as _;
        let mut failures = Vec::new();
        {
            if std::panic::AssertUnwindSafe(
                migration_combines_legacy_sources_with_backup_dedup_and_quarantine(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("migration_combines_legacy_sources_with_backup_dedup_and_quarantine");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                migration_cleans_a_partial_backup_temp_before_atomic_publication(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("migration_cleans_a_partial_backup_temp_before_atomic_publication");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                atomic_backup_publication_is_idempotent_for_verified_existing_bytes(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures
                    .push("atomic_backup_publication_is_idempotent_for_verified_existing_bytes");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                migration_stably_inserts_a_conversation_only_record_between_mirror_anchors(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push(
                    "migration_stably_inserts_a_conversation_only_record_between_mirror_anchors",
                );
            }
        }
        {
            if std::panic::AssertUnwindSafe(migration_aligns_legal_duplicates_by_source_order())
                .catch_unwind()
                .await
                .is_err()
            {
                failures.push("migration_aligns_legal_duplicates_by_source_order");
            }
        }
        {
            if std::panic::AssertUnwindSafe(opening_an_existing_store_is_idempotent())
                .catch_unwind()
                .await
                .is_err()
            {
                failures.push("opening_an_existing_store_is_idempotent");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                conversation_only_legacy_source_migrates_without_a_global_transcript(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures
                    .push("conversation_only_legacy_source_migrates_without_a_global_transcript");
            }
        }
        {
            if std::panic::AssertUnwindSafe(opening_rejects_a_modified_read_only_backup())
                .catch_unwind()
                .await
                .is_err()
            {
                failures.push("opening_rejects_a_modified_read_only_backup");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                incremental_migration_rejects_a_truncated_previous_backup(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("incremental_migration_rejects_a_truncated_previous_backup");
            }
        }

        #[cfg(unix)]
        {
            if std::panic::AssertUnwindSafe(
                migration_rejects_a_symlinked_legacy_conversations_directory(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("migration_rejects_a_symlinked_legacy_conversations_directory");
            }
        }

        #[cfg(unix)]
        {
            if std::panic::AssertUnwindSafe(
                runtime_operations_fail_closed_after_a_conversation_file_becomes_a_symlink(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push(
                    "runtime_operations_fail_closed_after_a_conversation_file_becomes_a_symlink",
                );
            }
        }

        #[cfg(unix)]
        {
            if std::panic::AssertUnwindSafe(
                atomic_new_file_write_never_follows_a_precreated_symlink(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("atomic_new_file_write_never_follows_a_precreated_symlink");
            }
        }

        #[cfg(unix)]
        {
            if std::panic::AssertUnwindSafe(
                session_generations_and_backups_use_private_permissions(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("session_generations_and_backups_use_private_permissions");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                appended_legacy_records_create_an_incremental_generation(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("appended_legacy_records_create_an_incremental_generation");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                incremental_migration_rebuilds_cross_generation_mirror_order(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("incremental_migration_rebuilds_cross_generation_mirror_order");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                incremental_migration_keeps_trailing_unique_legacy_after_native_events(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures
                    .push("incremental_migration_keeps_trailing_unique_legacy_after_native_events");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                a_truncated_tail_is_reparsed_after_legacy_completes_the_line(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("a_truncated_tail_is_reparsed_after_legacy_completes_the_line");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                mirrors_appended_in_separate_starts_preserve_legal_multiplicity(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("mirrors_appended_in_separate_starts_preserve_legal_multiplicity");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                migration_rejects_tool_result_without_a_preceding_call_before_publication(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push(
                    "migration_rejects_tool_result_without_a_preceding_call_before_publication",
                );
            }
        }
        {
            if std::panic::AssertUnwindSafe(migration_publishes_a_valid_tool_call_and_result_pair())
                .catch_unwind()
                .await
                .is_err()
            {
                failures.push("migration_publishes_a_valid_tool_call_and_result_pair");
            }
        }
        {
            if std::panic::AssertUnwindSafe(cloned_stores_share_one_serial_writer())
                .catch_unwind()
                .await
                .is_err()
            {
                failures.push("cloned_stores_share_one_serial_writer");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                separately_opened_handles_still_share_the_serial_writer(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("separately_opened_handles_still_share_the_serial_writer");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                opening_repairs_a_complete_event_that_only_lacks_the_final_newline(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("opening_repairs_a_complete_event_that_only_lacks_the_final_newline");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                opening_quarantines_and_truncates_an_incomplete_runtime_tail(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("opening_quarantines_and_truncates_an_incomplete_runtime_tail");
            }
        }
        {
            if std::panic::AssertUnwindSafe(opening_still_rejects_a_complete_bad_runtime_line())
                .catch_unwind()
                .await
                .is_err()
            {
                failures.push("opening_still_rejects_a_complete_bad_runtime_line");
            }
        }
        {
            if std::panic::AssertUnwindSafe(a_persisted_reservation_is_not_reused_after_restart())
                .catch_unwind()
                .await
                .is_err()
            {
                failures.push("a_persisted_reservation_is_not_reused_after_restart");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                incremental_migration_never_reuses_a_persisted_high_watermark(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("incremental_migration_never_reuses_a_persisted_high_watermark");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                recovery_rejects_an_untrusted_pending_delete_path_before_touching_disk(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures
                    .push("recovery_rejects_an_untrusted_pending_delete_path_before_touching_disk");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                opening_rejects_an_unsafe_migration_id_before_resolving_backup_paths(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures
                    .push("opening_rejects_an_unsafe_migration_id_before_resolving_backup_paths");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                opening_recovers_a_published_generation_when_pointer_was_not_written(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures
                    .push("opening_recovers_a_published_generation_when_pointer_was_not_written");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                pending_delete_failure_poisons_writer_until_restart_completes_recovery(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures
                    .push("pending_delete_failure_poisons_writer_until_restart_completes_recovery");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                failed_append_recovery_truncates_a_partial_line_without_poisoning(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("failed_append_recovery_truncates_a_partial_line_without_poisoning");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                failed_append_recovery_accepts_an_exact_complete_line_after_resync(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("failed_append_recovery_accepts_an_exact_complete_line_after_resync");
            }
        }
        {
            if std::panic::AssertUnwindSafe(
                failed_append_recovery_poisons_when_original_bytes_are_missing(),
            )
            .catch_unwind()
            .await
            .is_err()
            {
                failures.push("failed_append_recovery_poisons_when_original_bytes_are_missing");
            }
        }
        assert!(failures.is_empty(), "聚合测试失败：{}", failures.join(", "));
    }
}
