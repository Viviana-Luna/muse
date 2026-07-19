//! Persona 私有运行状态的 SQLite event/projection 与 Session v3 补投影。

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use muse_core::app::storage::{RuntimeStorageError, open_runtime_database};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::session::{SessionEventV3, SessionStore, SessionStoreError};

pub const PERSONA_EFFECTS_SCHEMA_VERSION: &str = "muse-persona-effects/v1";
const EMOTION_DECAY_PER_HOUR: u8 = 5;

#[derive(Debug)]
pub enum PersonaStateError {
    Storage(RuntimeStorageError),
    Sqlite(rusqlite::Error),
    Session(SessionStoreError),
    InvalidData(String),
}

impl std::fmt::Display for PersonaStateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(formatter),
            Self::Sqlite(error) => write!(formatter, "Persona 状态 SQLite 操作失败：{error}"),
            Self::Session(error) => error.fmt(formatter),
            Self::InvalidData(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for PersonaStateError {}

impl From<RuntimeStorageError> for PersonaStateError {
    fn from(value: RuntimeStorageError) -> Self {
        Self::Storage(value)
    }
}

impl From<rusqlite::Error> for PersonaStateError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sqlite(value)
    }
}

impl From<SessionStoreError> for PersonaStateError {
    fn from(value: SessionStoreError) -> Self {
        Self::Session(value)
    }
}

/// 最终 assistant 段产生的有界情绪候选。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PersonaEmotionEffect {
    pub emotion: String,
    pub intensity: u8,
    pub reason_code: String,
}

impl PersonaEmotionEffect {
    pub fn validate(&self) -> Result<(), PersonaStateError> {
        if !matches!(
            self.emotion.as_str(),
            "happy" | "sad" | "surprised" | "thinking" | "neutral" | "angry"
        ) {
            return Err(PersonaStateError::InvalidData(format!(
                "情绪候选 `{}` 不在允许枚举内。",
                self.emotion
            )));
        }
        if self.intensity > 100 {
            return Err(PersonaStateError::InvalidData(
                "情绪强度必须位于 0 到 100。".to_string(),
            ));
        }
        if !matches!(
            self.reason_code.as_str(),
            "positive_interaction"
                | "negative_interaction"
                | "surprise"
                | "deliberation"
                | "conflict"
                | "neutral"
        ) {
            return Err(PersonaStateError::InvalidData(format!(
                "情绪原因 `{}` 不在允许枚举内。",
                self.reason_code
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PersonaEffectsPayload {
    pub schema_version: String,
    pub persona_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emotion: Option<PersonaEmotionEffect>,
}

impl PersonaEffectsPayload {
    pub fn validate(&self) -> Result<(), PersonaStateError> {
        if self.schema_version != PERSONA_EFFECTS_SCHEMA_VERSION {
            return Err(PersonaStateError::InvalidData(format!(
                "Persona effect schema `{}` 不受支持。",
                self.schema_version
            )));
        }
        if self.persona_id.trim().is_empty() {
            return Err(PersonaStateError::InvalidData(
                "Persona effect 缺少 persona_id。".to_string(),
            ));
        }
        if let Some(emotion) = &self.emotion {
            emotion.validate()?;
        }
        Ok(())
    }
}

/// SQLite 中已提交的原始 Persona 情绪投影。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PersonaStateProjection {
    pub persona_id: String,
    pub emotion: String,
    pub intensity: u8,
    pub reason_code: String,
    pub source_conversation_id: String,
    pub source_turn_id: String,
    pub last_interaction_at: String,
    pub revision: u64,
}

/// 读取时纯计算的有效情绪；计算过程不得修改数据库 revision。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EffectivePersonaState {
    #[serde(flatten)]
    pub persisted: PersonaStateProjection,
    pub effective_emotion: String,
    pub effective_intensity: u8,
    pub evaluated_at: String,
}

impl PersonaStateProjection {
    pub fn effective_at(&self, now: DateTime<Utc>) -> EffectivePersonaState {
        let last = DateTime::parse_from_rfc3339(&self.last_interaction_at)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or(now);
        let elapsed_hours = now.signed_duration_since(last).num_hours().max(0) as u64;
        let decay = elapsed_hours
            .saturating_mul(u64::from(EMOTION_DECAY_PER_HOUR))
            .min(u64::from(u8::MAX)) as u8;
        let effective_intensity = self.intensity.saturating_sub(decay);
        EffectivePersonaState {
            persisted: self.clone(),
            effective_emotion: if effective_intensity == 0 {
                "neutral".to_string()
            } else {
                self.emotion.clone()
            },
            effective_intensity,
            evaluated_at: now.to_rfc3339(),
        }
    }
}

/// 只通过统一运行时数据库连接读写 Persona 状态。
pub struct PersonaStateStore {
    data_dir: PathBuf,
}

impl PersonaStateStore {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
        }
    }

    pub fn project_committed_event(
        &self,
        event: &SessionEventV3,
    ) -> Result<bool, PersonaStateError> {
        let Some(effects) = effects_from_committed_event(event)? else {
            return Ok(false);
        };
        let Some(emotion) = effects.emotion else {
            return Ok(false);
        };
        let turn_id = event.turn_id.as_deref().ok_or_else(|| {
            PersonaStateError::InvalidData("Persona effect 缺少 turn_id。".to_string())
        })?;
        let (_, mut connection) = open_runtime_database(&self.data_dir)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT persona_id, conversation_id, source_commit_seq, emotion, intensity,
                        reason_code, committed_at
                 FROM persona_state_event WHERE turn_id = ?1",
                [turn_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()?;
        let expected = (
            effects.persona_id.clone(),
            event.conversation_id.clone(),
            i64::try_from(event.commit_seq).unwrap_or(i64::MAX),
            emotion.emotion.clone(),
            i64::from(emotion.intensity),
            emotion.reason_code.clone(),
            event.time.clone(),
        );
        let inserted = existing.is_none();
        if let Some(existing) = existing {
            if existing != expected {
                return Err(PersonaStateError::InvalidData(format!(
                    "turn_id `{turn_id}` 对应的 Persona 状态事件与 canonical commit 冲突。"
                )));
            }
        } else {
            transaction.execute(
                "INSERT INTO persona_state_event(
                    turn_id, persona_id, conversation_id, source_commit_seq,
                    emotion, intensity, reason_code, committed_at
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    turn_id,
                    effects.persona_id,
                    event.conversation_id,
                    event.commit_seq,
                    emotion.emotion,
                    emotion.intensity,
                    emotion.reason_code,
                    event.time,
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO persona_state_projection(
                persona_id, emotion, intensity, reason_code, source_conversation_id,
                source_turn_id, last_interaction_at, revision
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(persona_id) DO UPDATE SET
                emotion = excluded.emotion,
                intensity = excluded.intensity,
                reason_code = excluded.reason_code,
                source_conversation_id = excluded.source_conversation_id,
                source_turn_id = excluded.source_turn_id,
                last_interaction_at = excluded.last_interaction_at,
                revision = excluded.revision
             WHERE excluded.revision > persona_state_projection.revision",
            params![
                effects.persona_id,
                emotion.emotion,
                emotion.intensity,
                emotion.reason_code,
                event.conversation_id,
                turn_id,
                event.time,
                event.commit_seq,
            ],
        )?;
        transaction.commit()?;
        Ok(inserted)
    }

    pub async fn recover_from_session_store(
        &self,
        store: &SessionStore,
    ) -> Result<usize, PersonaStateError> {
        let events = store.aggregate_events().await?;
        let mut recovered = 0usize;
        for event in events {
            if self.project_committed_event(&event)? {
                recovered = recovered.saturating_add(1);
            }
        }
        Ok(recovered)
    }

    pub fn projection(
        &self,
        persona_id: &str,
    ) -> Result<Option<PersonaStateProjection>, PersonaStateError> {
        let (_, connection) = open_runtime_database(&self.data_dir)?;
        connection
            .query_row(
                "SELECT persona_id, emotion, intensity, reason_code,
                        source_conversation_id, source_turn_id, last_interaction_at, revision
                 FROM persona_state_projection WHERE persona_id = ?1",
                [persona_id],
                |row| {
                    Ok(PersonaStateProjection {
                        persona_id: row.get(0)?,
                        emotion: row.get(1)?,
                        intensity: row.get::<_, i64>(2)?.clamp(0, 100) as u8,
                        reason_code: row.get(3)?,
                        source_conversation_id: row.get(4)?,
                        source_turn_id: row.get(5)?,
                        last_interaction_at: row.get(6)?,
                        revision: row.get::<_, i64>(7)?.max(0) as u64,
                    })
                },
            )
            .optional()
            .map_err(PersonaStateError::from)
    }
}

fn effects_from_committed_event(
    event: &SessionEventV3,
) -> Result<Option<PersonaEffectsPayload>, PersonaStateError> {
    if event.kind != "turn_committed" {
        return Ok(None);
    }
    let Some(value) = event.payload.get("persona_effects") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let effects: PersonaEffectsPayload =
        serde_json::from_value(value.clone()).map_err(|error| {
            PersonaStateError::InvalidData(format!(
                "turn_committed 的 persona_effects 无效：{error}"
            ))
        })?;
    effects.validate()?;
    Ok(Some(effects))
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;
    use tempfile::TempDir;

    use super::{PersonaStateProjection, PersonaStateStore};
    use crate::session::{SESSION_EVENT_SCHEMA_VERSION, SessionEventV3};

    fn committed_event(commit_seq: u64) -> SessionEventV3 {
        SessionEventV3 {
            schema_version: SESSION_EVENT_SCHEMA_VERSION.to_string(),
            event_id: format!("event-{commit_seq}"),
            commit_seq,
            conversation_id: "conversation-1".to_string(),
            turn_id: Some("turn-1".to_string()),
            turn_outcome: Some("committed".to_string()),
            kind: "turn_committed".to_string(),
            time: "2026-07-19T01:00:00Z".to_string(),
            payload: json!({
                "persona_effects": {
                    "schema_version": "muse-persona-effects/v1",
                    "persona_id": "persona-1",
                    "emotion": {
                        "emotion": "happy",
                        "intensity": 70,
                        "reason_code": "positive_interaction"
                    }
                }
            }),
            legacy_record: None,
            legacy_source: None,
        }
    }

    #[test]
    fn committed_effect_is_idempotent_and_projects_latest_state() {
        let temp = TempDir::new().unwrap();
        let store = PersonaStateStore::new(temp.path());
        let event = committed_event(7);

        assert!(store.project_committed_event(&event).unwrap());
        assert!(!store.project_committed_event(&event).unwrap());
        let projection = store.projection("persona-1").unwrap().unwrap();
        assert_eq!(projection.revision, 7);
        assert_eq!(projection.emotion, "happy");

        let (_, connection) = muse_core::app::storage::open_runtime_database(temp.path()).unwrap();
        connection
            .execute("DELETE FROM persona_state_projection", [])
            .unwrap();
        assert!(!store.project_committed_event(&event).unwrap());
        assert_eq!(
            store.projection("persona-1").unwrap().unwrap().revision,
            7,
            "重复执行 canonical event 应能重建缺失 projection"
        );
    }

    #[test]
    fn repeated_turn_id_rejects_conflicting_canonical_effect() {
        let temp = TempDir::new().unwrap();
        let store = PersonaStateStore::new(temp.path());
        let event = committed_event(7);
        store.project_committed_event(&event).unwrap();
        let mut conflict = event.clone();
        conflict.payload["persona_effects"]["emotion"]["intensity"] = json!(71);

        let error = store.project_committed_event(&conflict).unwrap_err();

        assert!(
            error.to_string().contains("canonical commit 冲突"),
            "同一 turn_id 不得静默覆盖不同状态事件"
        );
        assert_eq!(
            store.projection("persona-1").unwrap().unwrap().intensity,
            70
        );
    }

    #[test]
    fn committed_turn_without_emotion_keeps_existing_state_unchanged() {
        let temp = TempDir::new().unwrap();
        let store = PersonaStateStore::new(temp.path());
        let event = committed_event(7);
        store.project_committed_event(&event).unwrap();
        let mut paused = committed_event(8);
        paused.turn_id = Some("turn-2".to_string());
        paused.payload["persona_effects"] = json!({
            "schema_version": "muse-persona-effects/v1",
            "persona_id": "persona-1"
        });

        assert!(!store.project_committed_event(&paused).unwrap());
        assert_eq!(store.projection("persona-1").unwrap().unwrap().revision, 7);
    }

    #[test]
    fn decay_is_a_pure_read_calculation() {
        let projection = PersonaStateProjection {
            persona_id: "persona-1".to_string(),
            emotion: "happy".to_string(),
            intensity: 40,
            reason_code: "positive_interaction".to_string(),
            source_conversation_id: "conversation-1".to_string(),
            source_turn_id: "turn-1".to_string(),
            last_interaction_at: "2026-07-19T01:00:00Z".to_string(),
            revision: 7,
        };
        let now = Utc.with_ymd_and_hms(2026, 7, 19, 5, 0, 0).unwrap();

        let effective = projection.effective_at(now);

        assert_eq!(effective.effective_intensity, 20);
        assert_eq!(effective.persisted.revision, 7);
    }

    #[tokio::test]
    async fn restart_recovery_projects_only_canonical_committed_effects() {
        let temp = TempDir::new().unwrap();
        let (session_store, _) = crate::session::SessionStore::open(temp.path())
            .await
            .unwrap();
        let effects = json!({
            "schema_version": "muse-persona-effects/v1",
            "persona_id": "persona-1",
            "emotion": {
                "emotion": "thinking",
                "intensity": 60,
                "reason_code": "deliberation"
            }
        });
        session_store
            .append_event(
                "conversation-aborted",
                Some("turn-aborted".to_string()),
                "turn_aborted",
                json!({"persona_effects": effects.clone()}),
            )
            .await
            .unwrap();
        session_store
            .append_event(
                "conversation-cancelled",
                Some("turn-cancelled".to_string()),
                "turn_cancelled",
                json!({"persona_effects": effects.clone()}),
            )
            .await
            .unwrap();
        session_store
            .append_event(
                "conversation-committed",
                Some("turn-committed".to_string()),
                "turn_committed",
                json!({"persona_effects": effects}),
            )
            .await
            .unwrap();
        let store = PersonaStateStore::new(temp.path());
        assert!(store.projection("persona-1").unwrap().is_none());

        assert_eq!(
            store
                .recover_from_session_store(&session_store)
                .await
                .unwrap(),
            1
        );
        let projection = store.projection("persona-1").unwrap().unwrap();
        assert_eq!(projection.source_turn_id, "turn-committed");
        assert_eq!(projection.emotion, "thinking");
        assert_eq!(
            store
                .recover_from_session_store(&session_store)
                .await
                .unwrap(),
            0
        );
    }
}
