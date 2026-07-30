//! Session v3 JSONL 会话事实与可重建 SQLite 查询索引。

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::Utc;
use muse_core::app::storage::{RuntimeStorageError, open_runtime_database};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::ApprovalModePreset;
use crate::persona_state::{PersonaStateError, PersonaStateProjection, PersonaStateStore};
use crate::session::{
    SessionEventV3, SessionIndexIdentity, SessionMigrationReport, SessionStore, SessionStoreError,
    conversation_file_name,
};

pub const SESSION_METADATA_EVENT_KIND: &str = "session_metadata_updated";
pub const SESSION_APPROVAL_MODE_EVENT_KIND: &str = "session_approval_mode_updated";
const SESSION_METADATA_PAYLOAD_SCHEMA: &str = "muse-session-metadata/v2";
const SESSION_APPROVAL_MODE_PAYLOAD_SCHEMA: &str = "muse-session-approval-mode/v1";
const DEFAULT_SESSION_SUMMARY: &str = "未命名会话";
const MAX_INDEX_SUMMARY_CHARS: usize = 120;

#[derive(Debug)]
pub enum SessionRepositoryError {
    Session(SessionStoreError),
    Storage(RuntimeStorageError),
    Sqlite(rusqlite::Error),
    PersonaState(PersonaStateError),
    InvalidInput(String),
    InvalidData(String),
}

impl std::fmt::Display for SessionRepositoryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Session(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
            Self::Sqlite(error) => write!(formatter, "会话 SQLite 索引操作失败：{error}"),
            Self::PersonaState(error) => error.fmt(formatter),
            Self::InvalidInput(message) => formatter.write_str(message),
            Self::InvalidData(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for SessionRepositoryError {}

impl From<SessionStoreError> for SessionRepositoryError {
    fn from(value: SessionStoreError) -> Self {
        Self::Session(value)
    }
}

impl From<RuntimeStorageError> for SessionRepositoryError {
    fn from(value: RuntimeStorageError) -> Self {
        Self::Storage(value)
    }
}

impl From<rusqlite::Error> for SessionRepositoryError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sqlite(value)
    }
}

impl From<PersonaStateError> for SessionRepositoryError {
    fn from(value: PersonaStateError) -> Self {
        Self::PersonaState(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMetadata {
    pub persona_id: String,
    pub persona_name_snapshot: String,
    pub persona_version_snapshot: String,
    pub title: Option<String>,
    pub archived: bool,
    pub source_conversation_id: Option<String>,
    pub updated_at: String,
    pub revision: u64,
}

/// 会话级审批模式事实。revision 直接使用 Session v3 的全局提交序号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionApprovalMode {
    pub preset: ApprovalModePreset,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionListItem {
    pub conversation_id: String,
    pub persona_id: String,
    pub persona_name_snapshot: String,
    pub persona_version_snapshot: String,
    pub summary: String,
    pub source_conversation_id: Option<String>,
    pub records: u64,
    pub created_time: Option<String>,
    pub last_time: Option<String>,
    pub archived: bool,
    pub metadata_updated_at: Option<String>,
    pub metadata_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersonaRuntimeStateDeletion {
    pub workspace_state_rows: usize,
    pub state_event_rows: usize,
    pub state_projection_rows: usize,
}

struct MetadataSnapshot {
    persona_id: String,
    persona_name_snapshot: String,
    persona_version_snapshot: String,
    title: Option<String>,
    archived: bool,
    source_conversation_id: Option<String>,
}

#[derive(Debug, Clone)]
struct SessionProjection {
    conversation_id: String,
    event_file: String,
    title: Option<String>,
    archived: bool,
    source_conversation_id: Option<String>,
    persona_id: Option<String>,
    persona_name_snapshot: Option<String>,
    persona_version_snapshot: Option<String>,
    fallback_summary: String,
    record_count: u64,
    created_time: Option<String>,
    last_time: Option<String>,
    metadata_updated_at: Option<String>,
    metadata_revision: u64,
    last_commit_seq: u64,
    recoverable: bool,
}

impl SessionProjection {
    fn new(conversation_id: String, identity: &SessionIndexIdentity) -> Self {
        Self {
            event_file: event_file_path(identity, &conversation_id),
            conversation_id,
            title: None,
            archived: false,
            source_conversation_id: None,
            persona_id: None,
            persona_name_snapshot: None,
            persona_version_snapshot: None,
            fallback_summary: DEFAULT_SESSION_SUMMARY.to_string(),
            record_count: 0,
            created_time: None,
            last_time: None,
            metadata_updated_at: None,
            metadata_revision: 0,
            last_commit_seq: 0,
            recoverable: false,
        }
    }

    fn summary(&self) -> String {
        self.title
            .clone()
            .unwrap_or_else(|| self.fallback_summary.clone())
    }

    fn apply(&mut self, event: &SessionEventV3) -> Result<(), SessionRepositoryError> {
        self.record_count = self.record_count.saturating_add(1);
        self.last_commit_seq = self.last_commit_seq.max(event.commit_seq);
        if self.created_time.is_none() {
            self.created_time = Some(event.time.clone());
        }

        if event.kind == SESSION_METADATA_EVENT_KIND {
            apply_metadata_event(self, event)?;
            return Ok(());
        }

        if matches!(
            event.kind.as_str(),
            "turn_committed" | "turn_interrupted_with_effects" | "session_fork_snapshot"
        ) || (event.turn_id.is_none() && matches!(event.kind.as_str(), "user" | "assistant"))
        {
            self.recoverable = true;
        }

        self.last_time = Some(event.time.clone());
        if event.kind == "user"
            && self.fallback_summary == DEFAULT_SESSION_SUMMARY
            && let Some(content) = event.payload.get("content").and_then(Value::as_str)
        {
            self.fallback_summary = safe_index_summary(content);
        }
        if event.kind == "session_fork_snapshot"
            && let Some(source) = event
                .payload
                .get("source_conversation_id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        {
            self.source_conversation_id = Some(source.to_string());
        }
        Ok(())
    }
}

/// 统一协调 canonical JSONL 与可重建 SQLite 索引。
pub struct SessionRepository {
    data_dir: PathBuf,
    store: SessionStore,
    index_gate: Mutex<()>,
}

impl SessionRepository {
    pub async fn open(
        data_dir: impl AsRef<Path>,
    ) -> Result<(Self, SessionMigrationReport), SessionRepositoryError> {
        Self::open_internal(data_dir, true).await
    }

    pub(crate) async fn open_for_runtime(
        data_dir: impl AsRef<Path>,
    ) -> Result<(Self, SessionMigrationReport), SessionRepositoryError> {
        Self::open_internal(data_dir, false).await
    }

    async fn open_internal(
        data_dir: impl AsRef<Path>,
        recover_all_persona_states: bool,
    ) -> Result<(Self, SessionMigrationReport), SessionRepositoryError> {
        let data_dir = data_dir.as_ref().to_path_buf();
        let (store, report) = SessionStore::open(&data_dir).await?;
        let repository = Self {
            data_dir,
            store,
            index_gate: Mutex::new(()),
        };
        if let Err(error) = repository.ensure_index_current().await {
            tracing::warn!(%error, "会话仓储已打开，SQLite 索引将在列表访问时重试重建");
        }
        if recover_all_persona_states && let Err(error) = repository.recover_persona_state().await {
            tracing::warn!(%error, "Persona 状态补投影失败，将在下次读取时重试");
        }
        Ok((repository, report))
    }

    pub fn session_store(&self) -> &SessionStore {
        &self.store
    }

    /// 从 canonical Session v3 committed payload 幂等补齐 Persona 状态。
    pub async fn recover_persona_state(&self) -> Result<usize, SessionRepositoryError> {
        let _guard = self.index_gate.lock().await;
        Ok(PersonaStateStore::new(&self.data_dir)
            .recover_from_session_store(&self.store)
            .await?)
    }

    /// 启动时只恢复仍存在的 Persona，并在同一 SQLite 事务内清除
    /// 已删除 Persona 的工作区、事件和投影残留。
    pub async fn synchronize_persona_runtime_states(
        &self,
        persona_ids: &HashSet<String>,
    ) -> Result<usize, SessionRepositoryError> {
        let _guard = self.index_gate.lock().await;
        let recovered = PersonaStateStore::new(&self.data_dir)
            .recover_from_session_store_for_personas(&self.store, persona_ids)
            .await?;
        let (_, mut connection) = open_runtime_database(&self.data_dir)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stale_persona_ids = {
            let mut statement = transaction.prepare(
                "SELECT persona_id FROM persona_workspace_state
                 UNION
                 SELECT persona_id FROM persona_state_event
                 UNION
                 SELECT persona_id FROM persona_state_projection",
            )?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|persona_id| !persona_ids.contains(persona_id))
                .collect::<Vec<_>>()
        };
        for persona_id in stale_persona_ids {
            transaction.execute(
                "DELETE FROM persona_workspace_state WHERE persona_id = ?1",
                [&persona_id],
            )?;
            transaction.execute(
                "DELETE FROM persona_state_event WHERE persona_id = ?1",
                [&persona_id],
            )?;
            transaction.execute(
                "DELETE FROM persona_state_projection WHERE persona_id = ?1",
                [&persona_id],
            )?;
        }
        transaction.commit()?;
        Ok(recovered)
    }

    /// 投影刚刚可靠写入的 committed event。失败时 canonical commit 已经成立，
    /// 调用方不得追加冲突的 aborted 终态。
    pub fn project_committed_persona_state(
        &self,
        event: &SessionEventV3,
    ) -> Result<bool, SessionRepositoryError> {
        Ok(PersonaStateStore::new(&self.data_dir).project_committed_event(event)?)
    }

    /// 读取前先执行幂等补偿，避免上次进程在 JSONL commit 与 SQLite 投影之间退出。
    pub async fn persona_state(
        &self,
        persona_id: &str,
    ) -> Result<Option<PersonaStateProjection>, SessionRepositoryError> {
        let _guard = self.index_gate.lock().await;
        let persona_ids = HashSet::from([persona_id.to_string()]);
        PersonaStateStore::new(&self.data_dir)
            .recover_from_session_store_for_personas(&self.store, &persona_ids)
            .await?;
        Ok(PersonaStateStore::new(&self.data_dir).projection(persona_id)?)
    }

    pub async fn append_event(
        &self,
        conversation_id: impl Into<String>,
        turn_id: Option<String>,
        kind: impl Into<String>,
        payload: Value,
    ) -> Result<SessionEventV3, SessionRepositoryError> {
        let _guard = self.index_gate.lock().await;
        let previous_identity = self.store.index_identity().await?;
        let event = self
            .store
            .append_event(conversation_id, turn_id, kind, payload)
            .await?;
        self.update_index_after_committed_event(&event, &previous_identity)
            .await;
        Ok(event)
    }

    /// 追加会话级审批模式事件；YOLO 也会留下审计事实，但恢复时不会重新启用。
    pub async fn update_approval_mode(
        &self,
        conversation_id: &str,
        preset: ApprovalModePreset,
    ) -> Result<SessionApprovalMode, SessionRepositoryError> {
        validate_conversation_id(conversation_id)?;
        let event = self
            .append_event(
                conversation_id,
                None,
                SESSION_APPROVAL_MODE_EVENT_KIND,
                serde_json::json!({
                    "schema_version": SESSION_APPROVAL_MODE_PAYLOAD_SCHEMA,
                    "preset": preset,
                    "volatile": preset == ApprovalModePreset::Yolo,
                }),
            )
            .await?;
        Ok(SessionApprovalMode {
            preset,
            revision: event.commit_seq,
        })
    }

    /// 读取最后一个会话级审批模式。恢复路径会把历史 YOLO 收紧为手动审批。
    pub async fn approval_mode_for_resume(
        &self,
        conversation_id: &str,
    ) -> Result<SessionApprovalMode, SessionRepositoryError> {
        validate_conversation_id(conversation_id)?;
        let events = self.store.events_for_conversation(conversation_id).await?;
        let Some(event) = events
            .iter()
            .rev()
            .find(|event| event.kind == SESSION_APPROVAL_MODE_EVENT_KIND)
        else {
            return Ok(SessionApprovalMode {
                preset: ApprovalModePreset::Manual,
                revision: 0,
            });
        };
        let schema = event.payload.get("schema_version").and_then(Value::as_str);
        if schema != Some(SESSION_APPROVAL_MODE_PAYLOAD_SCHEMA) {
            return Err(SessionRepositoryError::InvalidData(
                "会话审批模式事件 schema 无效。".to_string(),
            ));
        }
        let preset = event
            .payload
            .get("preset")
            .cloned()
            .and_then(|value| serde_json::from_value::<ApprovalModePreset>(value).ok())
            .ok_or_else(|| {
                SessionRepositoryError::InvalidData("会话审批模式事件内容无效。".to_string())
            })?;
        Ok(SessionApprovalMode {
            preset: if preset == ApprovalModePreset::Yolo {
                ApprovalModePreset::Manual
            } else {
                preset
            },
            revision: event.commit_seq,
        })
    }

    pub async fn update_metadata(
        &self,
        conversation_id: &str,
        title: Option<Option<String>>,
        archived: Option<bool>,
    ) -> Result<SessionMetadata, SessionRepositoryError> {
        validate_conversation_id(conversation_id)?;
        let _guard = self.index_gate.lock().await;
        let previous_identity = self.store.index_identity().await?;
        let current = self
            .metadata_from_jsonl_locked(conversation_id, &previous_identity)
            .await?
            .ok_or_else(|| SessionRepositoryError::InvalidInput("会话不存在。".to_string()))?;
        let normalized_title = match title {
            Some(value) => normalize_title(value)?,
            None => current.title.clone(),
        };
        let next_archived = archived.unwrap_or(current.archived);
        if normalized_title == current.title && next_archived == current.archived {
            return Ok(current);
        }
        self.append_metadata_snapshot_locked(
            conversation_id,
            MetadataSnapshot {
                persona_id: current.persona_id,
                persona_name_snapshot: current.persona_name_snapshot,
                persona_version_snapshot: current.persona_version_snapshot,
                title: normalized_title,
                archived: next_archived,
                source_conversation_id: current.source_conversation_id,
            },
            &previous_identity,
        )
        .await
    }

    /// 在首个业务事件前创建 v2 Persona 绑定，或验证既有绑定不可变。
    pub async fn ensure_persona_binding(
        &self,
        conversation_id: &str,
        persona_id: &str,
        persona_name_snapshot: &str,
        persona_version_snapshot: &str,
    ) -> Result<SessionMetadata, SessionRepositoryError> {
        validate_conversation_id(conversation_id)?;
        let _guard = self.index_gate.lock().await;
        let identity = self.store.index_identity().await?;
        let events = self.store.events_for_conversation(conversation_id).await?;
        if events.is_empty() {
            return self
                .append_metadata_snapshot_locked(
                    conversation_id,
                    MetadataSnapshot {
                        persona_id: persona_id.to_string(),
                        persona_name_snapshot: persona_name_snapshot.to_string(),
                        persona_version_snapshot: persona_version_snapshot.to_string(),
                        title: None,
                        archived: false,
                        source_conversation_id: None,
                    },
                    &identity,
                )
                .await;
        }
        let mut projection = SessionProjection::new(conversation_id.to_string(), &identity);
        for event in &events {
            projection.apply(event)?;
        }
        let metadata = metadata_from_projection(projection)?;
        if metadata.persona_id != persona_id {
            return Err(SessionRepositoryError::InvalidInput(format!(
                "session_persona_mismatch：会话 `{conversation_id}` 属于 Persona `{}`，不能由 `{persona_id}` 继续。",
                metadata.persona_id
            )));
        }
        Ok(metadata)
    }

    /// 在分叉快照写入前原子建立目标会话的 v2 metadata。
    pub async fn initialize_fork(
        &self,
        conversation_id: &str,
        source_conversation_id: &str,
        persona_id: &str,
        persona_name_snapshot: &str,
        persona_version_snapshot: &str,
    ) -> Result<SessionMetadata, SessionRepositoryError> {
        validate_conversation_id(conversation_id)?;
        validate_conversation_id(source_conversation_id)?;
        let _guard = self.index_gate.lock().await;
        let identity = self.store.index_identity().await?;
        if !self
            .store
            .events_for_conversation(conversation_id)
            .await?
            .is_empty()
        {
            return Err(SessionRepositoryError::InvalidInput(
                "分叉目标会话已经存在。".to_string(),
            ));
        }
        self.append_metadata_snapshot_locked(
            conversation_id,
            MetadataSnapshot {
                persona_id: persona_id.to_string(),
                persona_name_snapshot: persona_name_snapshot.to_string(),
                persona_version_snapshot: persona_version_snapshot.to_string(),
                title: None,
                archived: false,
                source_conversation_id: Some(source_conversation_id.to_string()),
            },
            &identity,
        )
        .await
    }

    pub async fn metadata(
        &self,
        conversation_id: &str,
    ) -> Result<Option<SessionMetadata>, SessionRepositoryError> {
        validate_conversation_id(conversation_id)?;
        let _guard = self.index_gate.lock().await;
        let identity = self.store.index_identity().await?;
        self.metadata_from_jsonl_locked(conversation_id, &identity)
            .await
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionListItem>, SessionRepositoryError> {
        let _guard = self.index_gate.lock().await;
        self.ensure_index_current_locked().await?;
        let (_, connection) = open_runtime_database(&self.data_dir)?;
        let mut statement = connection.prepare(
            "SELECT conversation_id, persona_id, persona_name_snapshot, persona_version_snapshot,
                    summary, source_conversation_id, record_count, created_time, last_time,
                    archived, metadata_updated_at, metadata_revision
             FROM session_index
             WHERE recoverable = 1 AND persona_id IS NOT NULL
             ORDER BY COALESCE(last_time, created_time, '') DESC, conversation_id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(SessionListItem {
                conversation_id: row.get(0)?,
                persona_id: row.get(1)?,
                persona_name_snapshot: row.get(2)?,
                persona_version_snapshot: row.get(3)?,
                summary: row.get(4)?,
                source_conversation_id: row.get(5)?,
                records: row.get::<_, i64>(6)?.max(0) as u64,
                created_time: row.get(7)?,
                last_time: row.get(8)?,
                archived: row.get::<_, i64>(9)? != 0,
                metadata_updated_at: row.get(10)?,
                metadata_revision: row.get::<_, i64>(11)?.max(0) as u64,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub async fn latest_conversation_for_persona(
        &self,
        persona_id: &str,
    ) -> Result<Option<String>, SessionRepositoryError> {
        Ok(self
            .list_sessions()
            .await?
            .into_iter()
            .find(|item| item.persona_id == persona_id && !item.archived)
            .map(|item| item.conversation_id))
    }

    pub async fn preferred_conversation_for_persona(
        &self,
        persona_id: &str,
    ) -> Result<Option<String>, SessionRepositoryError> {
        let (_, connection) = open_runtime_database(&self.data_dir)?;
        let workspace_conversation = connection
            .query_row(
                "SELECT active_conversation_id FROM persona_workspace_state WHERE persona_id = ?1",
                [persona_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let sessions = self.list_sessions().await?;
        if let Some(conversation_id) = workspace_conversation
            && sessions.iter().any(|item| {
                item.conversation_id == conversation_id
                    && item.persona_id == persona_id
                    && !item.archived
            })
        {
            return Ok(Some(conversation_id));
        }
        Ok(sessions
            .into_iter()
            .find(|item| item.persona_id == persona_id && !item.archived)
            .map(|item| item.conversation_id))
    }

    pub async fn persona_session_count(
        &self,
        persona_id: &str,
    ) -> Result<usize, SessionRepositoryError> {
        Ok(self
            .list_sessions()
            .await?
            .into_iter()
            .filter(|item| item.persona_id == persona_id)
            .count())
    }

    pub fn workspace_state_exists(&self, persona_id: &str) -> Result<bool, SessionRepositoryError> {
        let (_, connection) = open_runtime_database(&self.data_dir)?;
        Ok(connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM persona_workspace_state WHERE persona_id = ?1)",
            [persona_id],
            |row| row.get(0),
        )?)
    }

    pub fn set_workspace_state(
        &self,
        persona_id: &str,
        conversation_id: &str,
    ) -> Result<(), SessionRepositoryError> {
        validate_conversation_id(conversation_id)?;
        let (_, connection) = open_runtime_database(&self.data_dir)?;
        connection.execute(
            "INSERT INTO persona_workspace_state(persona_id, active_conversation_id, updated_at)
             VALUES(?1, ?2, ?3)
             ON CONFLICT(persona_id) DO UPDATE SET
                active_conversation_id = excluded.active_conversation_id,
                updated_at = excluded.updated_at",
            params![persona_id, conversation_id, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn clear_workspace_state(&self, persona_id: &str) -> Result<(), SessionRepositoryError> {
        let (_, connection) = open_runtime_database(&self.data_dir)?;
        connection.execute(
            "DELETE FROM persona_workspace_state WHERE persona_id = ?1",
            [persona_id],
        )?;
        Ok(())
    }

    /// 原子清理 Persona 的全部 SQLite 派生状态；Session JSONL 保持原样，
    /// 继续作为只读历史与导出事实源。
    pub async fn delete_persona_runtime_state(
        &self,
        persona_id: &str,
    ) -> Result<PersonaRuntimeStateDeletion, SessionRepositoryError> {
        let _guard = self.index_gate.lock().await;
        let (_, mut connection) = open_runtime_database(&self.data_dir)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace_state_rows = transaction.execute(
            "DELETE FROM persona_workspace_state WHERE persona_id = ?1",
            [persona_id],
        )?;
        let state_event_rows = transaction.execute(
            "DELETE FROM persona_state_event WHERE persona_id = ?1",
            [persona_id],
        )?;
        let state_projection_rows = transaction.execute(
            "DELETE FROM persona_state_projection WHERE persona_id = ?1",
            [persona_id],
        )?;
        transaction.commit()?;
        Ok(PersonaRuntimeStateDeletion {
            workspace_state_rows,
            state_event_rows,
            state_projection_rows,
        })
    }

    pub async fn delete_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<usize, SessionRepositoryError> {
        let _guard = self.index_gate.lock().await;
        let previous_identity = self.store.index_identity().await?;
        let deleted = self.store.delete_conversation(conversation_id).await?;
        if deleted == 0 {
            return Ok(0);
        }
        let identity = match self.store.index_identity().await {
            Ok(identity) => identity,
            Err(error) => {
                tracing::warn!(%error, conversation_id, "会话 JSONL 已删除，无法刷新 SQLite 索引身份");
                return Ok(deleted);
            }
        };
        let result = (|| -> Result<(), SessionRepositoryError> {
            let (_, mut connection) = open_runtime_database(&self.data_dir)?;
            let transaction = connection.transaction()?;
            require_index_state(&transaction, &previous_identity)?;
            transaction.execute(
                "DELETE FROM session_index WHERE conversation_id = ?1",
                [conversation_id],
            )?;
            transaction.execute(
                "DELETE FROM persona_workspace_state WHERE active_conversation_id = ?1",
                [conversation_id],
            )?;
            write_index_state(&transaction, &identity)?;
            transaction.commit()?;
            Ok(())
        })();
        if let Err(error) = result {
            tracing::warn!(%error, conversation_id, "会话 JSONL 已删除，SQLite 索引将在下次访问时重建");
        }
        Ok(deleted)
    }

    async fn append_metadata_snapshot_locked(
        &self,
        conversation_id: &str,
        snapshot: MetadataSnapshot,
        previous_identity: &SessionIndexIdentity,
    ) -> Result<SessionMetadata, SessionRepositoryError> {
        let updated_at = Utc::now().to_rfc3339();
        let event = self
            .store
            .append_event(
                conversation_id,
                None,
                SESSION_METADATA_EVENT_KIND,
                serde_json::json!({
                    "schema_version": SESSION_METADATA_PAYLOAD_SCHEMA,
                    "persona_id": snapshot.persona_id,
                    "persona_name_snapshot": snapshot.persona_name_snapshot,
                    "persona_version_snapshot": snapshot.persona_version_snapshot,
                    "title": snapshot.title,
                    "archived": snapshot.archived,
                    "source_conversation_id": snapshot.source_conversation_id,
                    "updated_at": updated_at,
                }),
            )
            .await?;
        self.update_index_after_committed_event(&event, previous_identity)
            .await;
        Ok(SessionMetadata {
            persona_id: event.payload["persona_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            persona_name_snapshot: event.payload["persona_name_snapshot"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            persona_version_snapshot: event.payload["persona_version_snapshot"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            title: event
                .payload
                .get("title")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            archived: snapshot.archived,
            source_conversation_id: event
                .payload
                .get("source_conversation_id")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            updated_at,
            revision: event.commit_seq,
        })
    }

    async fn update_index_after_committed_event(
        &self,
        event: &SessionEventV3,
        previous_identity: &SessionIndexIdentity,
    ) {
        let result = async {
            let identity = self.store.index_identity().await?;
            let (_, mut connection) = open_runtime_database(&self.data_dir)?;
            let transaction = connection.transaction()?;
            require_index_state(&transaction, previous_identity)?;
            let mut projection = load_projection(&transaction, &identity, &event.conversation_id)?
                .unwrap_or_else(|| {
                    SessionProjection::new(event.conversation_id.clone(), &identity)
                });
            projection.event_file = event_file_path(&identity, &event.conversation_id);
            projection.apply(event)?;
            write_projection(&transaction, &projection)?;
            write_index_state(&transaction, &identity)?;
            transaction.commit()?;
            Ok::<(), SessionRepositoryError>(())
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(
                %error,
                conversation_id = %event.conversation_id,
                commit_seq = event.commit_seq,
                "Session v3 事实已提交，SQLite 会话索引将在下次访问时重建"
            );
        }
    }

    async fn metadata_from_jsonl_locked(
        &self,
        conversation_id: &str,
        identity: &SessionIndexIdentity,
    ) -> Result<Option<SessionMetadata>, SessionRepositoryError> {
        let events = self.store.events_for_conversation(conversation_id).await?;
        if events.is_empty() {
            return Ok(None);
        }
        let mut projection = SessionProjection::new(conversation_id.to_string(), identity);
        for event in &events {
            projection.apply(event)?;
        }
        Ok(Some(metadata_from_projection(projection)?))
    }

    async fn ensure_index_current(&self) -> Result<(), SessionRepositoryError> {
        let _guard = self.index_gate.lock().await;
        self.ensure_index_current_locked().await
    }

    async fn ensure_index_current_locked(&self) -> Result<(), SessionRepositoryError> {
        let identity = self.store.index_identity().await?;
        let (_, connection) = open_runtime_database(&self.data_dir)?;
        let current = connection
            .query_row(
                "SELECT generation_id, manifest_hash FROM session_index_state WHERE singleton = 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if current.as_ref()
            == Some(&(
                identity.generation_id.clone(),
                identity.manifest_hash.clone(),
            ))
        {
            return Ok(());
        }
        drop(connection);
        self.rebuild_index_locked(&identity).await
    }

    async fn rebuild_index_locked(
        &self,
        identity: &SessionIndexIdentity,
    ) -> Result<(), SessionRepositoryError> {
        let events = self.store.aggregate_events().await?;
        let mut projections = BTreeMap::<String, SessionProjection>::new();
        let mut invalid_conversations = std::collections::BTreeSet::<String>::new();
        for event in &events {
            if invalid_conversations.contains(&event.conversation_id) {
                continue;
            }
            let result = projections
                .entry(event.conversation_id.clone())
                .or_insert_with(|| SessionProjection::new(event.conversation_id.clone(), identity))
                .apply(event);
            if let Err(error) = result {
                tracing::warn!(
                    %error,
                    conversation_id = %event.conversation_id,
                    "忽略缺少有效 metadata v2 的测试 Session"
                );
                invalid_conversations.insert(event.conversation_id.clone());
            }
        }
        for conversation_id in invalid_conversations {
            projections.remove(&conversation_id);
        }
        let (_, mut connection) = open_runtime_database(&self.data_dir)?;
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM session_index", [])?;
        for projection in projections.values() {
            write_projection(&transaction, projection)?;
        }
        write_index_state(&transaction, identity)?;
        transaction.commit()?;
        Ok(())
    }
}

fn apply_metadata_event(
    projection: &mut SessionProjection,
    event: &SessionEventV3,
) -> Result<(), SessionRepositoryError> {
    if event.payload.get("schema_version").and_then(Value::as_str)
        != Some(SESSION_METADATA_PAYLOAD_SCHEMA)
    {
        return Err(SessionRepositoryError::InvalidData(format!(
            "session_persona_unbound：会话 `{}` 的元数据事件 `{}` 不是有效的 v2 绑定。",
            event.conversation_id, event.event_id
        )));
    }
    projection.persona_id = Some(required_metadata_string(event, "persona_id")?);
    projection.persona_name_snapshot =
        Some(required_metadata_string(event, "persona_name_snapshot")?);
    projection.persona_version_snapshot =
        Some(required_metadata_string(event, "persona_version_snapshot")?);
    projection.title = event
        .payload
        .get("title")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    projection.archived = event
        .payload
        .get("archived")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            SessionRepositoryError::InvalidData(format!(
                "会话 `{}` 的元数据事件缺少 archived。",
                event.conversation_id
            ))
        })?;
    projection.source_conversation_id = event
        .payload
        .get("source_conversation_id")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    projection.metadata_updated_at = Some(
        event
            .payload
            .get("updated_at")
            .and_then(Value::as_str)
            .unwrap_or(&event.time)
            .to_string(),
    );
    projection.metadata_revision = event.commit_seq;
    Ok(())
}

fn required_metadata_string(
    event: &SessionEventV3,
    key: &str,
) -> Result<String, SessionRepositoryError> {
    event
        .payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            SessionRepositoryError::InvalidData(format!(
                "session_persona_unbound：会话 `{}` 的 v2 元数据事件缺少 `{key}`。",
                event.conversation_id
            ))
        })
}

fn metadata_from_projection(
    projection: SessionProjection,
) -> Result<SessionMetadata, SessionRepositoryError> {
    let persona_id = projection.persona_id.ok_or_else(|| {
        SessionRepositoryError::InvalidData(format!(
            "session_persona_unbound：会话 `{}` 缺少 v2 Persona 绑定。",
            projection.conversation_id
        ))
    })?;
    Ok(SessionMetadata {
        persona_id,
        persona_name_snapshot: projection.persona_name_snapshot.unwrap_or_default(),
        persona_version_snapshot: projection.persona_version_snapshot.unwrap_or_default(),
        title: projection.title,
        archived: projection.archived,
        source_conversation_id: projection.source_conversation_id,
        updated_at: projection
            .metadata_updated_at
            .or(projection.created_time)
            .or(projection.last_time)
            .unwrap_or_default(),
        revision: projection.metadata_revision,
    })
}

fn load_projection(
    connection: &rusqlite::Connection,
    identity: &SessionIndexIdentity,
    conversation_id: &str,
) -> Result<Option<SessionProjection>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT event_file, title, archived, source_conversation_id, persona_id,
                    persona_name_snapshot, persona_version_snapshot, fallback_summary,
                    record_count, created_time, last_time, metadata_updated_at,
                    metadata_revision, last_commit_seq, recoverable
             FROM session_index WHERE conversation_id = ?1",
            [conversation_id],
            |row| {
                Ok(SessionProjection {
                    conversation_id: conversation_id.to_string(),
                    event_file: row.get(0)?,
                    title: row.get(1)?,
                    archived: row.get::<_, i64>(2)? != 0,
                    source_conversation_id: row.get(3)?,
                    persona_id: row.get(4)?,
                    persona_name_snapshot: row.get(5)?,
                    persona_version_snapshot: row.get(6)?,
                    fallback_summary: row.get(7)?,
                    record_count: row.get::<_, i64>(8)?.max(0) as u64,
                    created_time: row.get(9)?,
                    last_time: row.get(10)?,
                    metadata_updated_at: row.get(11)?,
                    metadata_revision: row.get::<_, i64>(12)?.max(0) as u64,
                    last_commit_seq: row.get::<_, i64>(13)?.max(0) as u64,
                    recoverable: row.get::<_, i64>(14)? != 0,
                })
            },
        )
        .optional()
        .map(|projection| {
            projection.map(|mut projection| {
                projection.event_file = event_file_path(identity, conversation_id);
                projection
            })
        })
}

fn write_projection(
    connection: &rusqlite::Connection,
    projection: &SessionProjection,
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO session_index(
             conversation_id, event_file, title, archived, source_conversation_id,
             persona_id, persona_name_snapshot, persona_version_snapshot,
             fallback_summary, summary, record_count, created_time, last_time,
             metadata_updated_at, metadata_revision, last_commit_seq, recoverable
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
         ON CONFLICT(conversation_id) DO UPDATE SET
             event_file = excluded.event_file,
             title = excluded.title,
             archived = excluded.archived,
             source_conversation_id = excluded.source_conversation_id,
             persona_id = excluded.persona_id,
             persona_name_snapshot = excluded.persona_name_snapshot,
             persona_version_snapshot = excluded.persona_version_snapshot,
             fallback_summary = excluded.fallback_summary,
             summary = excluded.summary,
             record_count = excluded.record_count,
             created_time = excluded.created_time,
             last_time = excluded.last_time,
             metadata_updated_at = excluded.metadata_updated_at,
             metadata_revision = excluded.metadata_revision,
             last_commit_seq = excluded.last_commit_seq,
             recoverable = excluded.recoverable",
        params![
            projection.conversation_id,
            projection.event_file,
            projection.title,
            i64::from(projection.archived),
            projection.source_conversation_id,
            projection.persona_id,
            projection.persona_name_snapshot,
            projection.persona_version_snapshot,
            projection.fallback_summary,
            projection.summary(),
            projection.record_count as i64,
            projection.created_time,
            projection.last_time,
            projection.metadata_updated_at,
            projection.metadata_revision as i64,
            projection.last_commit_seq as i64,
            i64::from(projection.recoverable),
        ],
    )?;
    Ok(())
}

fn write_index_state(
    connection: &rusqlite::Connection,
    identity: &SessionIndexIdentity,
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO session_index_state(singleton, generation_id, manifest_hash, indexed_at)
         VALUES (1, ?1, ?2, ?3)
         ON CONFLICT(singleton) DO UPDATE SET
             generation_id = excluded.generation_id,
             manifest_hash = excluded.manifest_hash,
             indexed_at = excluded.indexed_at",
        params![
            identity.generation_id,
            identity.manifest_hash,
            Utc::now().to_rfc3339()
        ],
    )?;
    Ok(())
}

fn require_index_state(
    connection: &rusqlite::Connection,
    expected: &SessionIndexIdentity,
) -> Result<(), SessionRepositoryError> {
    let current = connection
        .query_row(
            "SELECT generation_id, manifest_hash
             FROM session_index_state WHERE singleton = 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if current.as_ref()
        == Some(&(
            expected.generation_id.clone(),
            expected.manifest_hash.clone(),
        ))
    {
        return Ok(());
    }
    Err(SessionRepositoryError::InvalidData(
        "SQLite 会话索引状态已经过期，等待从 Session v3 JSONL 重建。".to_string(),
    ))
}

fn event_file_path(identity: &SessionIndexIdentity, conversation_id: &str) -> String {
    format!(
        "sessions/generations/{}/conversations/{}",
        identity.generation_id,
        conversation_file_name(conversation_id)
    )
}

fn normalize_title(value: Option<String>) -> Result<Option<String>, SessionRepositoryError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > 120 {
        return Err(SessionRepositoryError::InvalidInput(
            "会话标题不能超过 120 个字符".to_string(),
        ));
    }
    Ok(Some(value))
}

fn safe_index_summary(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = normalized.to_ascii_lowercase();
    if [
        "authorization:",
        "bearer ",
        "api_key=",
        "apikey=",
        "access_token=",
        "password=",
        "client_secret=",
        "github_pat_",
        "ghp_",
        "sk-",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "包含敏感内容的会话".to_string();
    }
    let mut chars = normalized.chars();
    let summary = chars
        .by_ref()
        .take(MAX_INDEX_SUMMARY_CHARS)
        .collect::<String>();
    if summary.is_empty() {
        DEFAULT_SESSION_SUMMARY.to_string()
    } else if chars.next().is_some() {
        format!("{summary}…")
    } else {
        summary
    }
}

fn validate_conversation_id(value: &str) -> Result<(), SessionRepositoryError> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(SessionRepositoryError::InvalidInput(
            "会话 ID 无效".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::json;
    use tempfile::tempdir;

    use crate::ApprovalModePreset;

    use super::SessionRepository;

    async fn bind(repository: &SessionRepository, conversation_id: &str) {
        repository
            .ensure_persona_binding(conversation_id, "persona-a", "角色 A", "1.0.0")
            .await
            .expect("应建立 v2 Persona 绑定");
    }

    #[tokio::test]
    async fn session_approval_mode_persists_auto_but_restores_yolo_as_manual() {
        let temp = tempdir().expect("应创建测试目录");
        let (repository, _) = SessionRepository::open(temp.path())
            .await
            .expect("应打开仓储");
        bind(&repository, "policy-chat").await;

        let auto = repository
            .update_approval_mode("policy-chat", ApprovalModePreset::Auto)
            .await
            .expect("应保存 AUTO 模式");
        let restored = repository
            .approval_mode_for_resume("policy-chat")
            .await
            .expect("应恢复 AUTO 模式");
        assert_eq!(restored.preset, ApprovalModePreset::Auto);
        assert_eq!(restored.revision, auto.revision);

        let yolo = repository
            .update_approval_mode("policy-chat", ApprovalModePreset::Yolo)
            .await
            .expect("应保存 YOLO 审计事实");
        let restored = repository
            .approval_mode_for_resume("policy-chat")
            .await
            .expect("应安全恢复历史 YOLO");
        assert_eq!(restored.preset, ApprovalModePreset::Manual);
        assert_eq!(restored.revision, yolo.revision);
    }

    #[tokio::test]
    async fn uncommitted_or_aborted_turn_is_never_listed_as_recoverable_session() {
        let temp = tempdir().expect("应创建测试目录");
        let (repository, _) = SessionRepository::open(temp.path())
            .await
            .expect("应打开仓储");
        bind(&repository, "failed-chat").await;
        repository
            .append_event(
                "failed-chat",
                Some("turn-1".to_string()),
                "user",
                json!({"content": "失败回合不得进入列表"}),
            )
            .await
            .expect("应写入私有用户事件");
        assert!(
            repository
                .list_sessions()
                .await
                .expect("应读取提交前列表")
                .is_empty()
        );
        repository
            .append_event(
                "failed-chat",
                Some("turn-1".to_string()),
                "turn_aborted",
                json!({"outcome": "aborted"}),
            )
            .await
            .expect("应写入失败终态");
        repository
            .append_event(
                "failed-chat",
                Some("turn-1".to_string()),
                "task_state",
                json!({"status": "failed"}),
            )
            .await
            .expect("应写入失败任务状态");
        assert!(
            repository
                .list_sessions()
                .await
                .expect("应读取失败后列表")
                .is_empty()
        );

        drop(repository);
        for suffix in ["", "-wal", "-shm"] {
            let path = temp.path().join(format!("runtime/muse.sqlite{suffix}"));
            if path.exists() {
                std::fs::remove_file(path).expect("应删除测试索引数据库");
            }
        }
        let (rebuilt, _) = SessionRepository::open(temp.path())
            .await
            .expect("应从 JSONL 重建索引");
        assert!(
            rebuilt
                .list_sessions()
                .await
                .expect("重建后仍应隐藏失败回合")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn turn_becomes_listable_only_after_committed_terminal_event() {
        let temp = tempdir().expect("应创建测试目录");
        let (repository, _) = SessionRepository::open(temp.path())
            .await
            .expect("应打开仓储");
        bind(&repository, "committed-chat").await;
        repository
            .append_event(
                "committed-chat",
                Some("turn-1".to_string()),
                "user",
                json!({"content": "提交后才可恢复"}),
            )
            .await
            .expect("应写入用户事件");
        assert!(
            repository
                .list_sessions()
                .await
                .expect("应读取提交前列表")
                .is_empty()
        );
        repository
            .append_event(
                "committed-chat",
                Some("turn-1".to_string()),
                "turn_committed",
                json!({"outcome": "committed"}),
            )
            .await
            .expect("应写入提交终态");
        let sessions = repository.list_sessions().await.expect("应读取提交后列表");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].summary, "提交后才可恢复");
        assert_eq!(sessions[0].records, 3);
    }

    #[tokio::test]
    async fn metadata_is_jsonl_fact_and_index_rebuilds_after_database_deletion() {
        let temp = tempdir().expect("应创建测试目录");
        let (repository, _) = SessionRepository::open(temp.path())
            .await
            .expect("应打开仓储");
        bind(&repository, "chat-1").await;
        repository
            .append_event("chat-1", None, "user", json!({"content": "第一章"}))
            .await
            .expect("应写入用户事件");
        let metadata = repository
            .update_metadata(
                "chat-1",
                Some(Some("  新   标题  ".to_string())),
                Some(true),
            )
            .await
            .expect("应写入元数据事件");
        assert_eq!(metadata.title.as_deref(), Some("新 标题"));
        assert!(metadata.archived);
        assert!(metadata.revision > 0);
        assert!(!temp.path().join("sessions/metadata.json").exists());

        drop(repository);
        for suffix in ["", "-wal", "-shm"] {
            let path = temp.path().join(format!("runtime/muse.sqlite{suffix}"));
            if path.exists() {
                std::fs::remove_file(path).expect("应删除测试索引数据库");
            }
        }
        let (rebuilt, _) = SessionRepository::open(temp.path())
            .await
            .expect("应重建索引");
        let sessions = rebuilt.list_sessions().await.expect("应读取重建列表");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].summary, "新 标题");
        assert!(sessions[0].archived);
        assert_eq!(sessions[0].metadata_revision, metadata.revision);
    }

    #[tokio::test]
    async fn identical_metadata_update_does_not_append_another_event() {
        let temp = tempdir().expect("应创建测试目录");
        let (repository, _) = SessionRepository::open(temp.path())
            .await
            .expect("应打开仓储");
        bind(&repository, "chat-1").await;
        repository
            .append_event("chat-1", None, "user", json!({"content": "hello"}))
            .await
            .expect("应写入事件");
        let first = repository
            .update_metadata("chat-1", Some(Some("标题".to_string())), Some(false))
            .await
            .expect("应写入元数据");
        let second = repository
            .update_metadata("chat-1", Some(Some("标题".to_string())), Some(false))
            .await
            .expect("相同更新应幂等");
        assert_eq!(first.revision, second.revision);
        let events = repository
            .session_store()
            .events_for_conversation("chat-1")
            .await
            .expect("应读取事件");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == super::SESSION_METADATA_EVENT_KIND)
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn stale_sqlite_index_never_blocks_jsonl_metadata_commit() {
        let temp = tempdir().expect("应创建测试目录");
        let (repository, _) = SessionRepository::open(temp.path())
            .await
            .expect("应打开仓储");
        bind(&repository, "chat-1").await;
        repository
            .append_event("chat-1", None, "user", json!({"content": "事实优先"}))
            .await
            .expect("应写入初始事实");

        let (_, connection) =
            muse_core::app::storage::open_runtime_database(temp.path()).expect("应打开测试索引");
        connection
            .execute(
                "UPDATE session_index_state SET manifest_hash = 'stale' WHERE singleton = 1",
                [],
            )
            .expect("应模拟过期索引");
        drop(connection);

        let metadata = repository
            .update_metadata("chat-1", Some(Some("JSONL 优先".to_string())), Some(true))
            .await
            .expect("过期 SQLite 不得阻止元数据事实提交");
        assert!(metadata.revision > 0);
        let events = repository
            .session_store()
            .events_for_conversation("chat-1")
            .await
            .expect("应读取 canonical 事件");
        assert_eq!(events.len(), 3);
        assert_eq!(
            events.last().map(|event| event.kind.as_str()),
            Some(super::SESSION_METADATA_EVENT_KIND)
        );
        let exported_metadata = repository
            .metadata("chat-1")
            .await
            .expect("JSONL 元数据读取不得依赖 SQLite")
            .expect("会话应存在");
        assert_eq!(exported_metadata.title.as_deref(), Some("JSONL 优先"));
        assert_eq!(exported_metadata.revision, metadata.revision);

        let sessions = repository
            .list_sessions()
            .await
            .expect("列表访问应从 JSONL 重建过期索引");
        assert_eq!(sessions[0].summary, "JSONL 优先");
        assert!(sessions[0].archived);
        assert_eq!(sessions[0].metadata_revision, metadata.revision);
    }

    #[tokio::test]
    async fn fork_metadata_and_delete_stay_consistent_with_rebuilt_index() {
        let temp = tempdir().expect("应创建测试目录");
        let (repository, _) = SessionRepository::open(temp.path())
            .await
            .expect("应打开仓储");
        let metadata = repository
            .initialize_fork("forked", "source", "persona-a", "角色 A", "1.0.0")
            .await
            .expect("应先写入分叉 v2 元数据");
        repository
            .append_event(
                "forked",
                None,
                "session_fork_snapshot",
                json!({"source_conversation_id": "source", "messages": []}),
            )
            .await
            .expect("应写入分叉快照");
        assert_eq!(metadata.source_conversation_id.as_deref(), Some("source"));
        assert!(metadata.revision > 0);
        let events = repository
            .session_store()
            .events_for_conversation("forked")
            .await
            .expect("应读取分叉事件");
        assert_eq!(
            events.first().map(|event| event.kind.as_str()),
            Some(super::SESSION_METADATA_EVENT_KIND)
        );

        assert_eq!(
            repository
                .delete_conversation("forked")
                .await
                .expect("应删除会话"),
            2
        );
        assert!(
            repository
                .list_sessions()
                .await
                .expect("应读取索引")
                .is_empty()
        );
        drop(repository);
        let (reopened, _) = SessionRepository::open(temp.path())
            .await
            .expect("应重新打开仓储");
        assert!(
            reopened
                .list_sessions()
                .await
                .expect("应重建空索引")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn secret_like_first_prompt_is_not_materialized_into_index_summary() {
        let temp = tempdir().expect("应创建测试目录");
        let (repository, _) = SessionRepository::open(temp.path())
            .await
            .expect("应打开仓储");
        bind(&repository, "secret-chat").await;
        repository
            .append_event(
                "secret-chat",
                None,
                "user",
                json!({"content": "请使用 sk-secret-value 调用接口"}),
            )
            .await
            .expect("应写入会话事实");
        let sessions = repository
            .list_sessions()
            .await
            .expect("应读取 SQLite 索引");
        assert_eq!(sessions[0].summary, "包含敏感内容的会话");
        assert!(!sessions[0].summary.contains("sk-secret-value"));
    }

    #[tokio::test]
    async fn v1_metadata_is_not_accepted_or_listed() {
        let temp = tempdir().expect("应创建测试目录");
        let (repository, _) = SessionRepository::open(temp.path())
            .await
            .expect("应打开仓储");
        repository
            .append_event(
                "legacy-test",
                None,
                super::SESSION_METADATA_EVENT_KIND,
                json!({
                    "schema_version": "muse-session-metadata/v1",
                    "title": "旧测试数据",
                    "archived": false,
                    "source_conversation_id": null,
                    "updated_at": "2026-07-16T00:00:00Z"
                }),
            )
            .await
            .expect("canonical JSONL 仍可保留无效测试事件供诊断");
        assert!(
            repository
                .list_sessions()
                .await
                .expect("列表应可重建")
                .is_empty()
        );
        assert!(repository.metadata("legacy-test").await.is_err());
    }

    #[tokio::test]
    async fn persona_binding_is_immutable() {
        let temp = tempdir().expect("应创建测试目录");
        let (repository, _) = SessionRepository::open(temp.path())
            .await
            .expect("应打开仓储");
        bind(&repository, "bound-chat").await;
        let error = repository
            .ensure_persona_binding("bound-chat", "persona-b", "角色 B", "1.0.0")
            .await
            .expect_err("既有会话不能原地切换 Persona");
        assert!(error.to_string().contains("session_persona_mismatch"));
    }

    #[tokio::test]
    async fn workspace_state_is_preferred_and_stale_state_falls_back_to_latest_session() {
        let temp = tempdir().expect("应创建测试目录");
        let (repository, _) = SessionRepository::open(temp.path())
            .await
            .expect("应打开仓储");
        for conversation_id in ["older-chat", "newer-chat"] {
            bind(&repository, conversation_id).await;
            repository
                .append_event(
                    conversation_id,
                    Some(format!("turn-{conversation_id}")),
                    "user",
                    json!({"content": conversation_id}),
                )
                .await
                .expect("应写入用户事件");
            repository
                .append_event(
                    conversation_id,
                    Some(format!("turn-{conversation_id}")),
                    "turn_committed",
                    json!({"outcome": "committed"}),
                )
                .await
                .expect("应写入提交终态");
        }
        repository
            .set_workspace_state("persona-a", "older-chat")
            .expect("应记录角色工作区会话");
        assert_eq!(
            repository
                .preferred_conversation_for_persona("persona-a")
                .await
                .expect("应读取工作区会话")
                .as_deref(),
            Some("older-chat")
        );
        repository
            .set_workspace_state("persona-a", "missing-chat")
            .expect("应模拟失效工作区状态");
        assert_eq!(
            repository
                .preferred_conversation_for_persona("persona-a")
                .await
                .expect("应回退最近会话")
                .as_deref(),
            Some("newer-chat")
        );
    }

    #[tokio::test]
    async fn persona_runtime_state_deletion_is_atomic_and_session_history_remains() {
        let temp = tempdir().expect("应创建测试目录");
        let (repository, _) = SessionRepository::open_for_runtime(temp.path())
            .await
            .expect("应打开运行时仓储");
        bind(&repository, "persona-delete-chat").await;
        let committed = repository
            .append_event(
                "persona-delete-chat",
                Some("turn-persona-delete".to_string()),
                "turn_committed",
                json!({
                    "persona_effects": {
                        "schema_version": "muse-persona-effects/v1",
                        "persona_id": "persona-a",
                        "emotion": {
                            "emotion": "happy",
                            "intensity": 60,
                            "reason_code": "positive_interaction"
                        }
                    }
                }),
            )
            .await
            .expect("应写入 Persona 状态 canonical 事件");
        repository
            .project_committed_persona_state(&committed)
            .expect("应投影 Persona 状态");
        repository
            .set_workspace_state("persona-a", "persona-delete-chat")
            .expect("应写入 Persona 工作区状态");

        let deleted = repository
            .delete_persona_runtime_state("persona-a")
            .await
            .expect("应原子清理 Persona 运行状态");

        assert_eq!(deleted.workspace_state_rows, 1);
        assert_eq!(deleted.state_event_rows, 1);
        assert_eq!(deleted.state_projection_rows, 1);
        let (_, connection) =
            muse_core::app::storage::open_runtime_database(temp.path()).expect("应打开测试数据库");
        for table in [
            "persona_workspace_state",
            "persona_state_event",
            "persona_state_projection",
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("应读取 Persona 状态表");
            assert_eq!(count, 0, "{table} 不得遗留已删除 Persona");
        }
        assert_eq!(
            repository
                .list_sessions()
                .await
                .expect("应保留关联 Session")
                .len(),
            1
        );

        drop(connection);
        drop(repository);
        let (reopened, _) = SessionRepository::open_for_runtime(temp.path())
            .await
            .expect("应模拟产品重启打开仓储");
        reopened
            .synchronize_persona_runtime_states(&HashSet::new())
            .await
            .expect("启动同步应忽略已删除 Persona 的 canonical 状态");
        let (_, connection) =
            muse_core::app::storage::open_runtime_database(temp.path()).expect("应重开测试数据库");
        for table in ["persona_state_event", "persona_state_projection"] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("应读取重启后的 Persona 状态表");
            assert_eq!(count, 0, "重启后 {table} 不得从只读 Session 复活");
        }
        assert_eq!(
            reopened
                .list_sessions()
                .await
                .expect("重启后应保留关联 Session")
                .len(),
            1
        );
    }
}
