//! Session v3 JSONL 会话事实与可重建 SQLite 查询索引。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use muse_core::app::storage::{RuntimeStorageError, open_runtime_database};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::session::{
    SessionEventV3, SessionIndexIdentity, SessionMigrationReport, SessionStore, SessionStoreError,
    conversation_file_name,
};

pub const SESSION_METADATA_EVENT_KIND: &str = "session_metadata_updated";
const SESSION_METADATA_PAYLOAD_SCHEMA: &str = "muse-session-metadata/v1";
const DEFAULT_SESSION_SUMMARY: &str = "未命名会话";
const MAX_INDEX_SUMMARY_CHARS: usize = 120;

#[derive(Debug)]
pub enum SessionRepositoryError {
    Session(SessionStoreError),
    Storage(RuntimeStorageError),
    Sqlite(rusqlite::Error),
    InvalidInput(String),
    InvalidData(String),
}

impl std::fmt::Display for SessionRepositoryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Session(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
            Self::Sqlite(error) => write!(formatter, "会话 SQLite 索引操作失败：{error}"),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMetadata {
    pub title: Option<String>,
    pub archived: bool,
    pub source_conversation_id: Option<String>,
    pub updated_at: String,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionListItem {
    pub conversation_id: String,
    pub summary: String,
    pub source_conversation_id: Option<String>,
    pub records: u64,
    pub created_time: Option<String>,
    pub last_time: Option<String>,
    pub archived: bool,
    pub metadata_updated_at: Option<String>,
    pub metadata_revision: u64,
}

#[derive(Debug, Clone)]
struct SessionProjection {
    conversation_id: String,
    event_file: String,
    title: Option<String>,
    archived: bool,
    source_conversation_id: Option<String>,
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
        Ok((repository, report))
    }

    pub fn session_store(&self) -> &SessionStore {
        &self.store
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
            normalized_title,
            next_archived,
            current.source_conversation_id,
            &previous_identity,
        )
        .await
    }

    pub async fn record_fork(
        &self,
        conversation_id: &str,
        source_conversation_id: &str,
    ) -> Result<SessionMetadata, SessionRepositoryError> {
        validate_conversation_id(conversation_id)?;
        validate_conversation_id(source_conversation_id)?;
        let _guard = self.index_gate.lock().await;
        let previous_identity = self.store.index_identity().await?;
        let current = self
            .metadata_from_jsonl_locked(conversation_id, &previous_identity)
            .await?
            .ok_or_else(|| SessionRepositoryError::InvalidInput("分叉会话不存在。".to_string()))?;
        if current.revision > 0
            && current.source_conversation_id.as_deref() == Some(source_conversation_id)
        {
            return Ok(current);
        }
        self.append_metadata_snapshot_locked(
            conversation_id,
            current.title,
            current.archived,
            Some(source_conversation_id.to_string()),
            &previous_identity,
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
            "SELECT conversation_id, summary, source_conversation_id, record_count,
                    created_time, last_time, archived, metadata_updated_at, metadata_revision
             FROM session_index
             WHERE recoverable = 1
             ORDER BY COALESCE(last_time, created_time, '') DESC, conversation_id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(SessionListItem {
                conversation_id: row.get(0)?,
                summary: row.get(1)?,
                source_conversation_id: row.get(2)?,
                records: row.get::<_, i64>(3)?.max(0) as u64,
                created_time: row.get(4)?,
                last_time: row.get(5)?,
                archived: row.get::<_, i64>(6)? != 0,
                metadata_updated_at: row.get(7)?,
                metadata_revision: row.get::<_, i64>(8)?.max(0) as u64,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
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
        title: Option<String>,
        archived: bool,
        source_conversation_id: Option<String>,
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
                    "title": title,
                    "archived": archived,
                    "source_conversation_id": source_conversation_id,
                    "updated_at": updated_at,
                }),
            )
            .await?;
        self.update_index_after_committed_event(&event, previous_identity)
            .await;
        Ok(SessionMetadata {
            title: event
                .payload
                .get("title")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            archived,
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
        Ok(Some(SessionMetadata {
            title: projection.title,
            archived: projection.archived,
            source_conversation_id: projection.source_conversation_id,
            updated_at: projection
                .metadata_updated_at
                .or(projection.created_time)
                .or(projection.last_time)
                .unwrap_or_default(),
            revision: projection.metadata_revision,
        }))
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
        for event in &events {
            projections
                .entry(event.conversation_id.clone())
                .or_insert_with(|| SessionProjection::new(event.conversation_id.clone(), identity))
                .apply(event)?;
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
            "会话 `{}` 的元数据事件 `{}` schema 无效。",
            event.conversation_id, event.event_id
        )));
    }
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

fn load_projection(
    connection: &rusqlite::Connection,
    identity: &SessionIndexIdentity,
    conversation_id: &str,
) -> Result<Option<SessionProjection>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT event_file, title, archived, source_conversation_id, fallback_summary,
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
                    fallback_summary: row.get(4)?,
                    record_count: row.get::<_, i64>(5)?.max(0) as u64,
                    created_time: row.get(6)?,
                    last_time: row.get(7)?,
                    metadata_updated_at: row.get(8)?,
                    metadata_revision: row.get::<_, i64>(9)?.max(0) as u64,
                    last_commit_seq: row.get::<_, i64>(10)?.max(0) as u64,
                    recoverable: row.get::<_, i64>(11)? != 0,
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
             fallback_summary, summary, record_count, created_time, last_time,
             metadata_updated_at, metadata_revision, last_commit_seq, recoverable
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(conversation_id) DO UPDATE SET
             event_file = excluded.event_file,
             title = excluded.title,
             archived = excluded.archived,
             source_conversation_id = excluded.source_conversation_id,
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
    use serde_json::json;
    use tempfile::tempdir;

    use super::SessionRepository;

    #[tokio::test]
    async fn uncommitted_or_aborted_turn_is_never_listed_as_recoverable_session() {
        let temp = tempdir().expect("应创建测试目录");
        let (repository, _) = SessionRepository::open(temp.path())
            .await
            .expect("应打开仓储");
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
        assert_eq!(sessions[0].records, 2);
    }

    #[tokio::test]
    async fn metadata_is_jsonl_fact_and_index_rebuilds_after_database_deletion() {
        let temp = tempdir().expect("应创建测试目录");
        let (repository, _) = SessionRepository::open(temp.path())
            .await
            .expect("应打开仓储");
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
            1
        );
    }

    #[tokio::test]
    async fn stale_sqlite_index_never_blocks_jsonl_metadata_commit() {
        let temp = tempdir().expect("应创建测试目录");
        let (repository, _) = SessionRepository::open(temp.path())
            .await
            .expect("应打开仓储");
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
        assert_eq!(events.len(), 2);
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
        repository
            .append_event(
                "forked",
                None,
                "session_fork_snapshot",
                json!({"source_conversation_id": "source", "messages": []}),
            )
            .await
            .expect("应写入分叉快照");
        let metadata = repository
            .record_fork("forked", "source")
            .await
            .expect("应写入分叉元数据事实");
        assert_eq!(metadata.source_conversation_id.as_deref(), Some("source"));
        assert!(metadata.revision > 0);
        let events = repository
            .session_store()
            .events_for_conversation("forked")
            .await
            .expect("应读取分叉事件");
        assert_eq!(
            events.last().map(|event| event.kind.as_str()),
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
}
