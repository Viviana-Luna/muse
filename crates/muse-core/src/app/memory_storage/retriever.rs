//! Persona 长期记忆的 SQLite FTS Retriever。
//!
//! 实现 domain 端口 `MemoryRetriever` 的两阶段语义：先由 FTS trigram 候选与
//! 连续覆盖率硬选池决定“能否进入结果”，再按有效权重（importance 枚举映射 ×
//! freshness 纯计算时效衰减）决定“先暴露哪几条”。每次新查询在独立读快照上
//! 物化整页排序结果，后续翻页通过 HMAC 保护的不透明游标读取冻结快照，
//! 同一 Turn 内的后续写入只影响新查询。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use rusqlite::{TransactionBehavior, params};

use super::authority::{CanonicalAuthorityGuard, SqliteMemoryDeletionAuthority};
use super::repository::{
    SqliteMemoryRepository, canonicalize_derivation_content, normalize_memory_fts_query,
    normalize_search_text, rfc3339_micros,
};
use crate::domain::memory::{
    MemoryCategory, MemoryChangeType, MemoryCursor, MemoryDeletionCheckRequest,
    MemoryDeletionDecision, MemoryDeletionSubject, MemoryError, MemoryErrorCode, MemoryId,
    MemoryImportance, MemoryPersonaScope, MemoryQueryItem, MemoryQueryPageReceipt,
    MemoryQueryParams, MemoryRetrievalRequest, MemoryRetriever, MemoryRevision, MemoryRevisionId,
};

// ===== 检索与分页数值常量（由固定中英文语料给出 F2 证据）=====
//
// 以下数值的证据来自本文件测试 `fixed_bilingual_corpus_evaluation` 对固定中文语料
// 与英文语料的实测输出；Turn 级状态由切片①接线，本模块只提供集中常量与
// 纯计量 helper。
pub const DEFAULT_MEMORY_QUERY_PAGE_SIZE: usize = 5;
pub const MAX_MEMORY_QUERY_PAGE_SIZE: usize = 8;
pub const MEMORY_QUERY_PAGE_TOKEN_BUDGET: usize = 400;
pub const MEMORY_QUERY_TURN_MAX_CALLS: usize = 6;
pub const MEMORY_QUERY_TURN_TOKEN_BUDGET: usize =
    MEMORY_QUERY_PAGE_TOKEN_BUDGET * MEMORY_QUERY_TURN_MAX_CALLS;
const MAX_QUERY_CHARS: usize = 120;
const CANDIDATE_LIMIT: usize = 100;
/// 硬选池门槛：规范化查询与候选当前正文的最长公共连续子串覆盖率下限。
const RELEVANCE_MIN_CONTIGUOUS_RATIO: f64 = 0.5;
const IMPORTANCE_WEIGHT_LOW: f64 = 1.0;
const IMPORTANCE_WEIGHT_NORMAL: f64 = 2.0;
const IMPORTANCE_WEIGHT_HIGH: f64 = 3.0;
/// freshness 半衰期；衰减是纯计算，检索与使用不回写 revision 或权重。
const FRESHNESS_HALF_LIFE_SECONDS: f64 = 30.0 * 24.0 * 3600.0;
const CURSOR_TTL_SECONDS: i64 = 1800;
const MAX_FROZEN_SNAPSHOTS: usize = 64;
/// items 之外的 JSON 外壳与最长游标开销；单条条目按完整 JSON 另行估算。
const PAGE_RECEIPT_TOKEN_OVERHEAD: usize = 48;

/// 游标完整性保护复用存储底座派生密钥域（authority 的 HMAC-SHA256）。
const CURSOR_HMAC_DOMAIN: &[u8] = b"muse-memory-query-cursor/v1";
/// 游标与查询绑定的 digest 域；绑定值不落游标明文，只留服务端快照。
const CURSOR_BINDING_DOMAIN: &[u8] = b"muse-memory-query-binding/v1";
const SNAPSHOT_ID_DOMAIN: &[u8] = b"muse-memory-query-snapshot/v1";
const CURSOR_PREFIX: &str = "mqc1";

const FTS_CANDIDATE_SQL: &str = "SELECT projection.memory_id, bm25(memory_fts)
     FROM memory_fts
     JOIN memory_search_projection AS projection
       ON projection.row_id = memory_fts.rowid
      AND projection.persona_id = ?2
     WHERE memory_fts MATCH ?1
       AND memory_fts.persona_id = ?2
     ORDER BY bm25(memory_fts), projection.memory_id
     LIMIT ?3";

/// SQLite FTS 版记忆检索器；真实实例由协调者在集成时注入运行时。
pub struct SqliteMemoryRetriever {
    repository: Arc<SqliteMemoryRepository>,
    /// 冻结快照只活在进程内存中：进程重启即全部失效，游标稳定报过期。
    snapshots: Mutex<BTreeMap<String, FrozenSnapshot>>,
    snapshot_sequence: AtomicU64,
    cursor_ttl_seconds: i64,
}

impl fmt::Debug for SqliteMemoryRetriever {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SqliteMemoryRetriever([去敏检索句柄])")
    }
}

impl SqliteMemoryRetriever {
    pub fn new(repository: Arc<SqliteMemoryRepository>) -> Self {
        Self {
            repository,
            snapshots: Mutex::new(BTreeMap::new()),
            snapshot_sequence: AtomicU64::new(0),
            cursor_ttl_seconds: CURSOR_TTL_SECONDS,
        }
    }

    #[cfg(test)]
    fn with_cursor_ttl_seconds(repository: Arc<SqliteMemoryRepository>, ttl_seconds: i64) -> Self {
        Self {
            cursor_ttl_seconds: ttl_seconds,
            ..Self::new(repository)
        }
    }

    /// 使一个 Turn 持有的查询快照立即过期。
    ///
    /// 同一快照后续签发的所有 cursor 都共享 snapshot_id，因此运行时在 Turn
    /// 结束时传入任一已记录 cursor 即可精准失效该查询，不影响并发 Turn。
    pub fn expire_cursor(&self, cursor: &MemoryCursor) -> Result<(), MemoryError> {
        let payload = parse_cursor(cursor.as_str())?;
        let expected_mac = self.cursor_mac(
            &payload.snapshot_id,
            payload.position,
            payload.expires_at_epoch,
        );
        if !constant_time_eq(payload.mac.as_bytes(), expected_mac.as_bytes()) {
            return Err(MemoryError::new(MemoryErrorCode::InvalidCursor));
        }
        self.lock_snapshots()?.remove(&payload.snapshot_id);
        Ok(())
    }

    fn retrieve_impl(
        &self,
        request: &MemoryRetrievalRequest,
    ) -> Result<MemoryQueryPageReceipt, MemoryError> {
        let scope = &request.scope;
        let params = &request.params;
        // 查询先经 FTS 专用规范化；有效字符不足 trigram 基线时要求模型改写。
        // 该规范化与派生键 canonicalize_derivation_content 严格分离，禁止混用。
        let normalized = normalize_memory_fts_query(&params.query)?;
        if normalized.chars().count() > MAX_QUERY_CHARS {
            return Err(MemoryError::new(MemoryErrorCode::QueryRejected));
        }
        // 显式历史查询只能针对单条记忆；与 as_of 叠加的时间-历史混合语义不开放。
        if params.include_history && params.memory_id.is_none() {
            return Err(MemoryError::new(MemoryErrorCode::QueryRejected));
        }
        if params.include_history && params.as_of.is_some() {
            return Err(MemoryError::new(MemoryErrorCode::QueryRejected));
        }

        let now = Utc::now();
        match &params.cursor {
            Some(cursor) => self.continue_page(scope, params, &normalized, cursor, now),
            None => self.first_page(scope, params, &normalized, now),
        }
    }

    fn first_page(
        &self,
        scope: &MemoryPersonaScope,
        params: &MemoryQueryParams,
        normalized: &str,
        now: DateTime<Utc>,
    ) -> Result<MemoryQueryPageReceipt, MemoryError> {
        let items = self.build_frozen_items(scope, params, normalized, now)?;
        if items.is_empty() {
            return Ok(MemoryQueryPageReceipt::new(Vec::new(), None));
        }
        let (page, next_position) = slice_page(&items, 0, page_size(params))?;
        if next_position >= items.len() {
            return Ok(MemoryQueryPageReceipt::new(page, None));
        }

        let snapshot_id = self.next_snapshot_id(scope, now);
        let binding = self.binding_digest(scope, params, normalized);
        let expires_at = now + Duration::seconds(self.cursor_ttl_seconds);
        {
            let mut snapshots = self.lock_snapshots()?;
            snapshots.retain(|_, snapshot| snapshot.expires_at > now);
            while snapshots.len() >= MAX_FROZEN_SNAPSHOTS {
                let Some(oldest) = snapshots
                    .iter()
                    .max_by(|left, right| right.1.created_at.cmp(&left.1.created_at))
                    .map(|(key, _)| key.clone())
                else {
                    break;
                };
                snapshots.remove(&oldest);
            }
            snapshots.insert(
                snapshot_id.clone(),
                FrozenSnapshot {
                    binding,
                    items,
                    created_at: now,
                    expires_at,
                },
            );
        }
        let next_cursor = self.build_cursor(&snapshot_id, next_position, expires_at)?;
        Ok(MemoryQueryPageReceipt::new(page, Some(next_cursor)))
    }

    fn continue_page(
        &self,
        scope: &MemoryPersonaScope,
        params: &MemoryQueryParams,
        normalized: &str,
        cursor: &MemoryCursor,
        now: DateTime<Utc>,
    ) -> Result<MemoryQueryPageReceipt, MemoryError> {
        let payload = parse_cursor(cursor.as_str())?;
        let expected_mac = self.cursor_mac(
            &payload.snapshot_id,
            payload.position,
            payload.expires_at_epoch,
        );
        // HMAC 不匹配覆盖一切篡改（含伪造的快照标识、位置与过期时间）。
        if !constant_time_eq(payload.mac.as_bytes(), expected_mac.as_bytes()) {
            return Err(MemoryError::new(MemoryErrorCode::InvalidCursor));
        }
        if now.timestamp() > payload.expires_at_epoch {
            return Err(MemoryError::new(MemoryErrorCode::CursorExpired));
        }
        let binding = self.binding_digest(scope, params, normalized);

        let mut snapshots = self.lock_snapshots()?;
        let Some(snapshot) = snapshots.get(&payload.snapshot_id) else {
            // HMAC 合法但快照不存在：TTL 驱逐、Turn 边界失效或进程重启。
            return Err(MemoryError::new(MemoryErrorCode::CursorExpired));
        };
        if snapshot.expires_at <= now {
            snapshots.remove(&payload.snapshot_id);
            return Err(MemoryError::new(MemoryErrorCode::CursorExpired));
        }
        // 绑定覆盖跨 Persona、改写查询、变更 include_history/as_of/memory_id 的混用。
        if snapshot.binding != binding {
            return Err(MemoryError::new(MemoryErrorCode::InvalidCursor));
        }
        let position = usize::try_from(payload.position)
            .map_err(|_| MemoryError::new(MemoryErrorCode::InvalidCursor))?;
        if position > snapshot.items.len() {
            return Err(MemoryError::new(MemoryErrorCode::InvalidCursor));
        }
        let (page, next_position) = slice_page(&snapshot.items, position, page_size(params))?;
        let next_cursor = if next_position < snapshot.items.len() {
            Some(self.build_cursor(&payload.snapshot_id, next_position, snapshot.expires_at)?)
        } else {
            None
        };
        if next_cursor.is_none() {
            snapshots.remove(&payload.snapshot_id);
        }
        Ok(MemoryQueryPageReceipt::new(page, next_cursor))
    }

    /// 在一次独立读快照（WAL 读事务）内物化本次查询的完整排序结果。
    fn build_frozen_items(
        &self,
        scope: &MemoryPersonaScope,
        params: &MemoryQueryParams,
        normalized: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<FrozenItem>, MemoryError> {
        let authority = self.repository.deletion_authority();
        // 锁序与 Repository 一致：先 authority 后主库，持有期间不重取。
        let authority_guard = authority.begin_guard()?;
        let mut connection = self.repository.open_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(repository_unavailable)?;
        let items = match &params.memory_id {
            Some(memory_id) if params.include_history => {
                self.history_items(&transaction, &authority_guard, authority, scope, memory_id)?
            }
            Some(memory_id) => self.direct_items(
                &transaction,
                &authority_guard,
                authority,
                scope,
                memory_id,
                params.as_of.as_deref(),
            )?,
            None => self.relevance_items(
                &transaction,
                &authority_guard,
                authority,
                scope,
                normalized,
                params.as_of.as_deref(),
                now,
            )?,
        };
        transaction.commit().map_err(repository_unavailable)?;
        authority_guard.finish()?;
        Ok(items)
    }

    /// 相关性模式：FTS 候选 -> 硬淘汰 -> 有效权重排序。
    #[allow(clippy::too_many_arguments)]
    fn relevance_items(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        authority_guard: &CanonicalAuthorityGuard<'_>,
        authority: &SqliteMemoryDeletionAuthority,
        scope: &MemoryPersonaScope,
        normalized: &str,
        as_of: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Vec<FrozenItem>, MemoryError> {
        let mut statement = transaction
            .prepare(FTS_CANDIDATE_SQL)
            .map_err(repository_unavailable)?;
        let candidates = statement
            .query_map(
                params![normalized, scope.persona_id(), CANDIDATE_LIMIT as i64],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
            )
            .map_err(repository_unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(repository_unavailable)?;
        drop(statement);

        let as_of_micros = as_of.map(rfc3339_micros).transpose()?;
        // as_of 查询的权重与内容都按该时刻求值，保证时间点查询结果可重现。
        let reference_micros = as_of_micros.unwrap_or_else(|| now.timestamp_micros());
        let query_chars: Vec<char> = normalized.chars().collect();

        let mut items = Vec::with_capacity(candidates.len());
        for (memory_id, bm25) in candidates {
            let memory_id = MemoryId(memory_id);
            // 候选只含当前 Persona、entry active、当前 revision；deleted 与跨
            // Persona 在 SQL 与 load_current 两层都被排除。
            let Some(record) = self
                .repository
                .load_current_on(transaction, scope, &memory_id)?
            else {
                continue;
            };
            if candidate_blocked(
                authority_guard,
                authority,
                scope,
                &memory_id,
                &record.current_revision.content,
            )? {
                continue;
            }
            // 硬淘汰：FTS 是第一层相关候选，连续覆盖率再对未来可能放宽的 FTS
            // 表达式和异常投影做防御；重排或凑字干扰即使获得高权重也不得进入。
            let content_chars: Vec<char> = normalize_search_text(&record.current_revision.content)
                .chars()
                .collect();
            if !passes_hard_relevance(&query_chars, &content_chars) {
                continue;
            }
            // as_of：相关性仍由当前正文判定（FTS 投影只含当前 revision），展示
            // 该时刻可读的 revision；该时刻不存在可读 revision 的候选被排除。
            let revision = match as_of_micros {
                None => record.current_revision.clone(),
                Some(at) => {
                    let history =
                        self.repository
                            .revision_history_on(transaction, scope, &memory_id)?;
                    match revision_valid_at(history, at)? {
                        Some(revision) => revision,
                        None => continue,
                    }
                }
            };
            let freshness_micros = rfc3339_micros(&record.entry.freshness_at)?;
            let effective_weight = importance_weight(record.entry.importance)
                * freshness_decay(reference_micros - freshness_micros);
            items.push(FrozenItem::new(
                &record.entry,
                revision,
                effective_weight,
                bm25,
                freshness_micros,
            )?);
        }
        // 排序主键为有效权重；同权重按相关度、更新时间、稳定 ID 保证确定顺序。
        items.sort_by(compare_frozen_items);
        Ok(items)
    }

    /// 直接读取模式：模型显式指定 memory_id，不做相关性门槛。
    fn direct_items(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        authority_guard: &CanonicalAuthorityGuard<'_>,
        authority: &SqliteMemoryDeletionAuthority,
        scope: &MemoryPersonaScope,
        memory_id: &MemoryId,
        as_of: Option<&str>,
    ) -> Result<Vec<FrozenItem>, MemoryError> {
        let record = self
            .repository
            .load_current_on(transaction, scope, memory_id)?
            .ok_or_else(|| MemoryError::new(MemoryErrorCode::MemoryNotFound))?;
        if candidate_blocked(
            authority_guard,
            authority,
            scope,
            memory_id,
            &record.current_revision.content,
        )? {
            return Err(MemoryError::new(MemoryErrorCode::MemoryNotFound));
        }
        let revision = match as_of {
            None => record.current_revision.clone(),
            Some(value) => {
                let at = rfc3339_micros(value)?;
                let history = self
                    .repository
                    .revision_history_on(transaction, scope, memory_id)?;
                // 该时刻不存在模型可读 revision（例如正处于 corrected 区间）时
                // 返回空页，而不是把不可读内容暴露给模型。
                match revision_valid_at(history, at)? {
                    Some(revision) => revision,
                    None => return Ok(Vec::new()),
                }
            }
        };
        let freshness_micros = rfc3339_micros(&record.entry.freshness_at)?;
        Ok(vec![FrozenItem::new(
            &record.entry,
            revision,
            0.0,
            0.0,
            freshness_micros,
        )?])
    }

    /// 显式历史模式：返回 current 与 superseded 历史，带 change_type/change_reason/
    /// valid_to；corrected 永远不进入模型读取面。
    fn history_items(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        authority_guard: &CanonicalAuthorityGuard<'_>,
        authority: &SqliteMemoryDeletionAuthority,
        scope: &MemoryPersonaScope,
        memory_id: &MemoryId,
    ) -> Result<Vec<FrozenItem>, MemoryError> {
        let record = self
            .repository
            .load_current_on(transaction, scope, memory_id)?
            .ok_or_else(|| MemoryError::new(MemoryErrorCode::MemoryNotFound))?;
        if candidate_blocked(
            authority_guard,
            authority,
            scope,
            memory_id,
            &record.current_revision.content,
        )? {
            return Err(MemoryError::new(MemoryErrorCode::MemoryNotFound));
        }
        let freshness_micros = rfc3339_micros(&record.entry.freshness_at)?;
        let history = self
            .repository
            .revision_history_on(transaction, scope, memory_id)?;
        let mut items = Vec::with_capacity(history.len());
        for revision in history {
            if !revision.state.is_model_readable() {
                continue;
            }
            if candidate_blocked(
                authority_guard,
                authority,
                scope,
                memory_id,
                &revision.content,
            )? {
                continue;
            }
            items.push(FrozenItem::new(
                &record.entry,
                revision,
                0.0,
                0.0,
                freshness_micros,
            )?);
        }
        if items.is_empty() {
            return Err(MemoryError::new(MemoryErrorCode::MemoryNotFound));
        }
        // 历史按 valid_from 倒序（最新在前），同刻按 recorded_at、revision_id 稳定排序。
        items.sort_by(|left, right| {
            right
                .valid_from_micros
                .cmp(&left.valid_from_micros)
                .then_with(|| right.recorded_micros.cmp(&left.recorded_micros))
                .then_with(|| right.revision_id.0.cmp(&left.revision_id.0))
        });
        Ok(items)
    }

    fn binding_digest(
        &self,
        scope: &MemoryPersonaScope,
        params: &MemoryQueryParams,
        normalized: &str,
    ) -> [u8; 32] {
        let include_history = [u8::from(params.include_history)];
        let as_of = params.as_of.as_deref().unwrap_or("");
        let memory_id = params
            .memory_id
            .as_ref()
            .map(|memory_id| memory_id.0.as_str())
            .unwrap_or("");
        self.repository.deletion_authority().keyed_digest(
            CURSOR_BINDING_DOMAIN,
            &[
                scope.persona_id().as_bytes(),
                normalized.as_bytes(),
                &include_history,
                as_of.as_bytes(),
                memory_id.as_bytes(),
            ],
        )
    }

    fn cursor_mac(&self, snapshot_id: &str, position: u64, expires_at_epoch: i64) -> String {
        let digest = self.repository.deletion_authority().keyed_digest(
            CURSOR_HMAC_DOMAIN,
            &[
                snapshot_id.as_bytes(),
                &position.to_be_bytes(),
                &expires_at_epoch.to_be_bytes(),
            ],
        );
        hex_encode(&digest)
    }

    fn build_cursor(
        &self,
        snapshot_id: &str,
        position: usize,
        expires_at: DateTime<Utc>,
    ) -> Result<MemoryCursor, MemoryError> {
        let position = u64::try_from(position)
            .map_err(|_| MemoryError::new(MemoryErrorCode::RepositoryUnavailable))?;
        let expires_at_epoch = expires_at.timestamp();
        let mac = self.cursor_mac(snapshot_id, position, expires_at_epoch);
        MemoryCursor::from_runtime(format!(
            "{CURSOR_PREFIX}.{snapshot_id}.{position}.{expires_at_epoch}.{mac}"
        ))
        .map_err(|_| MemoryError::new(MemoryErrorCode::RepositoryUnavailable))
    }

    fn next_snapshot_id(&self, scope: &MemoryPersonaScope, now: DateTime<Utc>) -> String {
        let sequence = self.snapshot_sequence.fetch_add(1, Ordering::Relaxed);
        let digest = self.repository.deletion_authority().keyed_digest(
            SNAPSHOT_ID_DOMAIN,
            &[
                scope.persona_id().as_bytes(),
                &sequence.to_be_bytes(),
                &now.timestamp_micros().to_be_bytes(),
            ],
        );
        hex_encode(&digest[..8])
    }

    fn lock_snapshots(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, FrozenSnapshot>>, MemoryError> {
        self.snapshots
            .lock()
            .map_err(|_| MemoryError::new(MemoryErrorCode::RepositoryUnavailable))
    }
}

impl MemoryRetriever for SqliteMemoryRetriever {
    fn retrieve(
        &self,
        request: &MemoryRetrievalRequest,
    ) -> Result<MemoryQueryPageReceipt, MemoryError> {
        self.retrieve_impl(request)
    }
}

/// 单个 Turn 的记忆查询计量器；状态创建、持有与 Tool 接线由运行时负责。
///
/// 调用方在执行 Retriever 前调用 `reserve_query_call`，使被拒绝的无效查询也消耗
/// 调用次数；成功后、加入当前私有工作副本前再调用 `consume_page_tokens`。超出
/// 调用次数、单页 Token 或 Turn 总 Token 任一边界时稳定返回预算错误。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryQueryBudget {
    consumed_calls: usize,
    consumed_tokens: usize,
}

impl MemoryQueryBudget {
    pub const fn new() -> Self {
        Self {
            consumed_calls: 0,
            consumed_tokens: 0,
        }
    }

    pub const fn consumed_calls(&self) -> usize {
        self.consumed_calls
    }

    pub const fn consumed_tokens(&self) -> usize {
        self.consumed_tokens
    }

    pub fn reserve_query_call(&mut self) -> Result<(), MemoryError> {
        let next_calls = self
            .consumed_calls
            .checked_add(1)
            .ok_or_else(query_budget_exceeded)?;
        if next_calls > MEMORY_QUERY_TURN_MAX_CALLS {
            return Err(query_budget_exceeded());
        }
        self.consumed_calls = next_calls;
        Ok(())
    }

    pub fn consume_page_tokens(
        &mut self,
        page: &MemoryQueryPageReceipt,
    ) -> Result<usize, MemoryError> {
        let page_tokens = estimated_memory_query_page_tokens(page)?;
        let next_tokens = self
            .consumed_tokens
            .checked_add(page_tokens)
            .ok_or_else(query_budget_exceeded)?;
        if page_tokens > MEMORY_QUERY_PAGE_TOKEN_BUDGET
            || next_tokens > MEMORY_QUERY_TURN_TOKEN_BUDGET
        {
            return Err(query_budget_exceeded());
        }
        self.consumed_tokens = next_tokens;
        Ok(page_tokens)
    }
}

/// 对模型实际可见的完整页 JSON 做统一保守估算，供分页与 Turn 预算复用。
pub fn estimated_memory_query_page_tokens(
    page: &MemoryQueryPageReceipt,
) -> Result<usize, MemoryError> {
    let json = serde_json::to_string(page).map_err(repository_unavailable)?;
    Ok(estimate_tokens(&json))
}

/// 一次新查询物化出的冻结快照；同一页内容在本 Turn 内不变。
struct FrozenSnapshot {
    binding: [u8; 32],
    items: Vec<FrozenItem>,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

/// 冻结快照中的一条有序结果；排序键在快照创建时固化。
struct FrozenItem {
    memory_id: MemoryId,
    revision_id: MemoryRevisionId,
    category: MemoryCategory,
    content: String,
    importance: MemoryImportance,
    event_time: Option<String>,
    recorded_at: String,
    valid_from: String,
    valid_to: Option<String>,
    change_type: MemoryChangeType,
    change_reason: String,
    effective_weight: f64,
    relevance: f64,
    freshness_micros: i64,
    valid_from_micros: i64,
    recorded_micros: i64,
}

impl FrozenItem {
    fn new(
        entry: &crate::domain::memory::MemoryEntry,
        revision: MemoryRevision,
        effective_weight: f64,
        relevance: f64,
        freshness_micros: i64,
    ) -> Result<Self, MemoryError> {
        let valid_from_micros = rfc3339_micros(&revision.valid_from)?;
        let recorded_micros = rfc3339_micros(&revision.recorded_at)?;
        Ok(Self {
            memory_id: entry.memory_id.clone(),
            revision_id: revision.revision_id,
            category: entry.category,
            content: revision.content,
            importance: entry.importance,
            event_time: revision.event_time,
            recorded_at: revision.recorded_at,
            valid_from: revision.valid_from,
            valid_to: revision.valid_to,
            change_type: revision.change_type,
            change_reason: revision.change_reason,
            effective_weight,
            relevance,
            freshness_micros,
            valid_from_micros,
            recorded_micros,
        })
    }

    fn to_query_item(&self) -> MemoryQueryItem {
        MemoryQueryItem {
            memory_id: self.memory_id.clone(),
            revision_id: self.revision_id.clone(),
            category: self.category,
            content: self.content.clone(),
            importance: self.importance,
            event_time: self.event_time.clone(),
            recorded_at: self.recorded_at.clone(),
            valid_from: self.valid_from.clone(),
            valid_to: self.valid_to.clone(),
            change_type: self.change_type,
            change_reason: self.change_reason.clone(),
        }
    }

    fn estimated_tokens(&self) -> Result<usize, MemoryError> {
        let json = serde_json::to_string(&self.to_query_item()).map_err(repository_unavailable)?;
        Ok(estimate_tokens(&json))
    }
}

fn compare_frozen_items(left: &FrozenItem, right: &FrozenItem) -> std::cmp::Ordering {
    right
        .effective_weight
        .total_cmp(&left.effective_weight)
        .then_with(|| left.relevance.total_cmp(&right.relevance))
        .then_with(|| right.freshness_micros.cmp(&left.freshness_micros))
        .then_with(|| left.memory_id.0.cmp(&right.memory_id.0))
}

/// 条数与单页 Token 双重限制。单条已经超预算时直接淘汰并推进位置，禁止用
/// “至少返回一条”突破上限；该类异常长记忆不会阻塞后续较短候选的分页。
fn slice_page(
    items: &[FrozenItem],
    start: usize,
    page_size: usize,
) -> Result<(Vec<MemoryQueryItem>, usize), MemoryError> {
    let mut page = Vec::new();
    let mut tokens = PAGE_RECEIPT_TOKEN_OVERHEAD;
    let mut position = start;
    while let Some(item) = items.get(position) {
        if page.len() >= page_size {
            break;
        }
        let estimate = item.estimated_tokens()?;
        if PAGE_RECEIPT_TOKEN_OVERHEAD + estimate > MEMORY_QUERY_PAGE_TOKEN_BUDGET {
            position += 1;
            continue;
        }
        if tokens + estimate > MEMORY_QUERY_PAGE_TOKEN_BUDGET {
            break;
        }
        tokens += estimate;
        page.push(item.to_query_item());
        position += 1;
    }
    Ok((page, position))
}

fn page_size(params: &MemoryQueryParams) -> usize {
    params
        .limit
        .map(|limit| usize::min(limit as usize, MAX_MEMORY_QUERY_PAGE_SIZE))
        .unwrap_or(DEFAULT_MEMORY_QUERY_PAGE_SIZE)
}

/// importance 受限枚举到权重基值的映射。
fn importance_weight(importance: MemoryImportance) -> f64 {
    match importance {
        MemoryImportance::Low => IMPORTANCE_WEIGHT_LOW,
        MemoryImportance::Normal => IMPORTANCE_WEIGHT_NORMAL,
        MemoryImportance::High => IMPORTANCE_WEIGHT_HIGH,
    }
}

/// freshness 半衰期指数衰减；纯计算，读取与使用不加权、不回写。
fn freshness_decay(age_micros: i64) -> f64 {
    let age_seconds = (age_micros.max(0) as f64) / 1_000_000.0;
    2.0_f64.powf(-age_seconds / FRESHNESS_HALF_LIFE_SECONDS)
}

/// 模型读取面只接受 current/superseded；corrected 在此被排除。
/// 同一时刻存在多条可读 revision 视为数据损坏并 fail closed。
fn revision_valid_at(
    history: Vec<MemoryRevision>,
    at_micros: i64,
) -> Result<Option<MemoryRevision>, MemoryError> {
    let mut best: Option<(i64, MemoryRevision)> = None;
    for revision in history {
        if !revision.state.is_model_readable() {
            continue;
        }
        let valid_from = rfc3339_micros(&revision.valid_from)?;
        let valid_to = revision
            .valid_to
            .as_deref()
            .map(rfc3339_micros)
            .transpose()?;
        if valid_from <= at_micros && valid_to.is_none_or(|valid_to| at_micros < valid_to) {
            if best.is_some() {
                return Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable));
            }
            best = Some((valid_from, revision));
        }
    }
    Ok(best.map(|(_, revision)| revision))
}

/// 读取路径的删除权威阻断检查，与 Repository 读取口径一致：
/// Persona、memory 与该条正文派生键任一被阻断即不可读。
fn candidate_blocked(
    authority_guard: &CanonicalAuthorityGuard<'_>,
    authority: &SqliteMemoryDeletionAuthority,
    scope: &MemoryPersonaScope,
    memory_id: &MemoryId,
    content: &str,
) -> Result<bool, MemoryError> {
    let check = MemoryDeletionCheckRequest::new(BTreeSet::from([
        MemoryDeletionSubject::Persona {
            persona_id: scope.persona_id().to_string(),
        },
        MemoryDeletionSubject::Memory {
            persona_id: scope.persona_id().to_string(),
            memory_id: memory_id.clone(),
        },
        MemoryDeletionSubject::Derivation {
            persona_id: scope.persona_id().to_string(),
            derivation_key: authority.derivation_key(
                scope.persona_id(),
                &canonicalize_derivation_content(content),
            ),
        },
    ]))?;
    Ok(matches!(
        authority_guard.check(&check)?,
        MemoryDeletionDecision::Blocked { .. }
    ))
}

/// 最长公共连续子串长度（按字符）；硬选池覆盖率的纯计算基础。
fn longest_common_substring_len(query: &[char], content: &[char]) -> usize {
    if query.is_empty() || content.is_empty() {
        return 0;
    }
    let mut previous = vec![0_usize; content.len() + 1];
    let mut best = 0;
    for query_char in query {
        let mut current = vec![0_usize; content.len() + 1];
        for (content_index, content_char) in content.iter().enumerate() {
            if query_char == content_char {
                current[content_index + 1] = previous[content_index] + 1;
                best = best.max(current[content_index + 1]);
            }
        }
        previous = current;
    }
    best
}

fn passes_hard_relevance(query: &[char], content: &[char]) -> bool {
    if query.is_empty() {
        return false;
    }
    longest_common_substring_len(query, content) as f64 / query.len() as f64
        >= RELEVANCE_MIN_CONTIGUOUS_RATIO
}

/// 粗粒度 Token 估计：CJK 与 emoji 按 1 token/字符，ASCII 按 1 token/4 字符。
fn estimate_tokens(text: &str) -> usize {
    let mut ascii = 0_usize;
    let mut wide = 0_usize;
    for character in text.chars() {
        if character.is_ascii() {
            ascii += 1;
        } else {
            wide += 1;
        }
    }
    wide + ascii.div_ceil(4)
}

#[derive(Debug)]
struct ParsedCursor {
    snapshot_id: String,
    position: u64,
    expires_at_epoch: i64,
    mac: String,
}

fn parse_cursor(value: &str) -> Result<ParsedCursor, MemoryError> {
    let invalid = || MemoryError::new(MemoryErrorCode::InvalidCursor);
    let parts: Vec<&str> = value.split('.').collect();
    if parts.len() != 5 || parts[0] != CURSOR_PREFIX {
        return Err(invalid());
    }
    let snapshot_id = parts[1];
    if snapshot_id.len() != 16 || !snapshot_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    let position = parts[2].parse::<u64>().map_err(|_| invalid())?;
    let expires_at_epoch = parts[3].parse::<i64>().map_err(|_| invalid())?;
    let mac = parts[4];
    if mac.len() != 64 || !mac.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    Ok(ParsedCursor {
        snapshot_id: snapshot_id.to_string(),
        position,
        expires_at_epoch,
        mac: mac.to_string(),
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("写入 String 不会失败");
    }
    encoded
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

fn query_budget_exceeded() -> MemoryError {
    MemoryError::new(MemoryErrorCode::QueryBudgetExceeded)
}

fn repository_unavailable<T>(_error: T) -> MemoryError {
    MemoryError::new(MemoryErrorCode::RepositoryUnavailable)
}

#[cfg(test)]
mod tests;
