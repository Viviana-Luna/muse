//! Persona 长期记忆的 SQLite FTS Retriever。
//!
//! 实现 domain 端口 `MemoryRetriever` 的两阶段语义：先由 FTS trigram 候选与
//! 连续覆盖率硬选池决定“能否进入结果”，再按有效权重（importance 枚举映射 ×
//! freshness 纯计算时效衰减）决定“先暴露哪几条”。每次新查询在独立读快照上
//! 物化整页排序结果，后续翻页通过绑定真实 Turn 与全部规范化条件的 HMAC
//! 不透明句柄读取冻结快照。同 Turn 同 cursor 是幂等重试；普通 update 保持冻结
//! 语义，correct 与 durable delete 则在待发送续页复核时使旧正文无正文失败。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration as MonotonicDuration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
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
    MemoryQueryParams, MemoryRetrievalFilters, MemoryRetrievalRequest, MemoryRetrievalTurn,
    MemoryRetriever, MemoryRevision, MemoryRevisionId, validate_memory_change_reason,
    validate_memory_content,
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
const FTS_SCAN_BATCH_SIZE: usize = 100;
const MAX_CANDIDATES_SCANNED: usize = 512;
/// 硬选池门槛：规范化查询与候选当前正文的最长公共连续子串覆盖率下限。
const RELEVANCE_MIN_CONTIGUOUS_RATIO: f64 = 0.5;
const IMPORTANCE_WEIGHT_LOW: f64 = 1.0;
const IMPORTANCE_WEIGHT_NORMAL: f64 = 2.0;
const IMPORTANCE_WEIGHT_HIGH: f64 = 3.0;
/// freshness 半衰期；衰减是纯计算，检索与使用不回写 revision 或权重。
const FRESHNESS_HALF_LIFE_SECONDS: f64 = 30.0 * 24.0 * 3600.0;
const CURSOR_TTL_SECONDS: i64 = 1800;
const MAX_FROZEN_SNAPSHOTS: usize = 64;
const MAX_FROZEN_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
const MAX_PERSONA_FROZEN_SNAPSHOTS: usize = 16;
const MAX_PERSONA_FROZEN_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
const MAX_TURN_FROZEN_SNAPSHOTS: usize = MEMORY_QUERY_TURN_MAX_CALLS;
const MAX_TURN_FROZEN_SNAPSHOT_BYTES: usize = 512 * 1024;
/// items 之外的 JSON 外壳与最长游标开销；单条条目按完整 JSON 另行估算。
const PAGE_RECEIPT_TOKEN_OVERHEAD: usize = 48;

/// 游标完整性保护复用存储底座派生密钥域（authority 的 HMAC-SHA256）。
const CURSOR_HMAC_DOMAIN: &[u8] = b"muse-memory-query-cursor/v1";
/// 游标与查询绑定的 digest 域；绑定值不落游标明文，只留服务端快照。
const CURSOR_BINDING_DOMAIN: &[u8] = b"muse-memory-query-binding/v1";
const SNAPSHOT_ID_DOMAIN: &[u8] = b"muse-memory-query-snapshot/v1";
const CURSOR_HANDLE_DOMAIN: &[u8] = b"muse-memory-query-page-handle/v1";
const RETRIEVAL_SORT_VERSION: &[u8] = b"effective-weight-v1";
const CURSOR_PREFIX: &str = "mqc1";

const FTS_CANDIDATE_SQL: &str = "SELECT projection.memory_id, bm25(memory_fts)
     FROM memory_fts
     JOIN memory_search_projection AS projection
       ON projection.row_id = memory_fts.rowid
      AND projection.persona_id = ?2
     JOIN memory_entry AS entry
       ON entry.persona_id = projection.persona_id
      AND entry.memory_id = projection.memory_id
      AND entry.state = 'active'
     WHERE memory_fts MATCH ?1
       AND memory_fts.persona_id = ?2
     ORDER BY CASE entry.importance
                WHEN 'high' THEN 0
                WHEN 'normal' THEN 1
                ELSE 2
              END,
              entry.freshness_at DESC,
              bm25(memory_fts),
              projection.memory_id
     LIMIT ?3 OFFSET ?4";

const AS_OF_CANDIDATE_SQL: &str = "SELECT memory_id
     FROM memory_entry
     WHERE persona_id = ?1 AND state = 'active'
     ORDER BY CASE importance
                WHEN 'high' THEN 0
                WHEN 'normal' THEN 1
                ELSE 2
              END,
              freshness_at DESC,
              memory_id
     LIMIT ?2 OFFSET ?3";

/// SQLite FTS 版记忆检索器；真实实例由协调者在集成时注入运行时。
pub struct SqliteMemoryRetriever {
    repository: Arc<SqliteMemoryRepository>,
    /// 冻结快照只活在进程内存中：进程重启即全部失效，游标稳定报过期。
    snapshots: Mutex<BTreeMap<String, FrozenSnapshot>>,
    snapshot_sequence: AtomicU64,
    cursor_ttl_seconds: i64,
    snapshot_limits: SnapshotBudgetLimits,
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
            snapshot_limits: SnapshotBudgetLimits::production(),
        }
    }

    #[cfg(test)]
    fn with_cursor_ttl_seconds(repository: Arc<SqliteMemoryRepository>, ttl_seconds: i64) -> Self {
        Self {
            cursor_ttl_seconds: ttl_seconds,
            ..Self::new(repository)
        }
    }

    #[cfg(test)]
    fn with_snapshot_limits(
        repository: Arc<SqliteMemoryRepository>,
        snapshot_limits: SnapshotBudgetLimits,
    ) -> Self {
        Self {
            snapshot_limits,
            ..Self::new(repository)
        }
    }

    /// 使一个 Turn 持有的查询快照立即过期。
    ///
    /// 同一快照后续签发的所有 cursor 都共享 snapshot_id，因此运行时在 Turn
    /// 结束时传入任一已记录 cursor 即可精准失效该查询，不影响并发 Turn。
    pub fn expire_cursor(&self, cursor: &MemoryCursor) -> Result<(), MemoryError> {
        let payload = parse_cursor(cursor.as_str())?;
        let mut snapshots = self.lock_snapshots()?;
        let snapshot = snapshots
            .get(&payload.snapshot_id)
            .ok_or_else(|| MemoryError::new(MemoryErrorCode::CursorExpired))?;
        let expected_mac = self.cursor_mac(
            &payload.snapshot_id,
            &payload.cursor_handle,
            &snapshot.binding,
        );
        if !constant_time_eq(payload.mac.as_bytes(), expected_mac.as_bytes()) {
            return Err(MemoryError::new(MemoryErrorCode::InvalidCursor));
        }
        snapshots.remove(&payload.snapshot_id);
        Ok(())
    }

    /// 失效一个真实 Turn 拥有的全部冻结查询，避免漏记单个 cursor 时跨 Turn 残留。
    pub fn expire_turn(&self, turn: &MemoryRetrievalTurn) -> Result<usize, MemoryError> {
        let mut snapshots = self.lock_snapshots()?;
        let before = snapshots.len();
        snapshots.retain(|_, snapshot| snapshot.owner.turn != *turn);
        Ok(before - snapshots.len())
    }

    fn retrieve_impl(
        &self,
        request: &MemoryRetrievalRequest,
    ) -> Result<MemoryQueryPageReceipt, MemoryError> {
        let params = request.params();
        // 即使未来新增内部调用者，也必须在规范化和评分前重验原始输入上限。
        params.validate()?;
        // 查询先经 FTS 专用规范化；有效字符不足 trigram 基线时要求模型改写。
        // 该规范化与派生键 canonicalize_derivation_content 严格分离，禁止混用。
        let normalized = normalize_memory_fts_query(&params.query)?;
        // 显式历史查询只能针对单条记忆；与 as_of 叠加的时间-历史混合语义不开放。
        if params.include_history && params.memory_id.is_none() {
            return Err(MemoryError::new(MemoryErrorCode::QueryRejected));
        }
        if params.include_history && params.as_of.is_some() {
            return Err(MemoryError::new(MemoryErrorCode::QueryRejected));
        }

        let now = Utc::now();
        match &params.cursor {
            Some(cursor) => self.continue_page(request, &normalized, cursor, Instant::now()),
            None => self.first_page(request, &normalized, now),
        }
    }

    fn first_page(
        &self,
        request: &MemoryRetrievalRequest,
        normalized: &str,
        now: DateTime<Utc>,
    ) -> Result<MemoryQueryPageReceipt, MemoryError> {
        let params = request.params();
        let items = self.build_frozen_items(request, normalized, now)?;
        if items.is_empty() {
            return Ok(MemoryQueryPageReceipt::new(Vec::new(), None));
        }
        let (page, next_position) = slice_page(&items, 0, page_size(params))?;
        if next_position >= items.len() {
            return Ok(MemoryQueryPageReceipt::new(page, None));
        }

        let snapshot_id = self.next_snapshot_id(request, now);
        let binding = self.binding_digest(request, normalized)?;
        let monotonic_now = Instant::now();
        let expires_at = monotonic_now
            .checked_add(self.cursor_ttl())
            .ok_or_else(|| MemoryError::new(MemoryErrorCode::RepositoryUnavailable))?;
        let byte_size = frozen_snapshot_bytes(&items)?;
        let (cursor_handle, next_cursor) =
            self.build_cursor(&snapshot_id, next_position, &binding)?;
        {
            let mut snapshots = self.lock_snapshots()?;
            snapshots.retain(|_, snapshot| snapshot.expires_at > monotonic_now);
            let owner = SnapshotOwner::from_request(request);
            ensure_snapshot_budget(&snapshots, &owner, byte_size, self.snapshot_limits)?;
            snapshots.insert(
                snapshot_id.clone(),
                FrozenSnapshot {
                    binding,
                    items,
                    expires_at,
                    byte_size,
                    owner,
                    cursor_positions: BTreeMap::from([(cursor_handle, next_position)]),
                },
            );
        }
        Ok(MemoryQueryPageReceipt::new(page, Some(next_cursor)))
    }

    fn continue_page(
        &self,
        request: &MemoryRetrievalRequest,
        normalized: &str,
        cursor: &MemoryCursor,
        monotonic_now: Instant,
    ) -> Result<MemoryQueryPageReceipt, MemoryError> {
        let scope = request.scope();
        let params = request.params();
        let payload = parse_cursor(cursor.as_str())?;
        let binding = self.binding_digest(request, normalized)?;
        let expected_mac = self.cursor_mac(&payload.snapshot_id, &payload.cursor_handle, &binding);
        // HMAC 覆盖不透明句柄以及完整规范化查询与真实 Turn 绑定。
        if !constant_time_eq(payload.mac.as_bytes(), expected_mac.as_bytes()) {
            return Err(MemoryError::new(MemoryErrorCode::InvalidCursor));
        }

        let (position, pending) = {
            let mut snapshots = self.lock_snapshots()?;
            snapshots.retain(|_, snapshot| snapshot.expires_at > monotonic_now);
            let snapshot = snapshots
                .get(&payload.snapshot_id)
                .ok_or_else(|| MemoryError::new(MemoryErrorCode::CursorExpired))?;
            if snapshot.binding != binding || snapshot.owner != SnapshotOwner::from_request(request)
            {
                return Err(MemoryError::new(MemoryErrorCode::InvalidCursor));
            }
            let position = *snapshot
                .cursor_positions
                .get(&payload.cursor_handle)
                .ok_or_else(|| MemoryError::new(MemoryErrorCode::InvalidCursor))?;
            (position, snapshot.items[position..].to_vec())
        };

        // 普通 update 会把旧 revision 置为 superseded，仍保留冻结分页语义；
        // correct 会置为 corrected，delete 会命中权威，二者都必须在发送前失效。
        let authority = self.repository.deletion_authority();
        let authority_guard = authority.begin_guard()?;
        let mut connection = self.repository.open_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(repository_unavailable)?;
        let pending_valid = self.pending_items_still_readable(
            &transaction,
            &authority_guard,
            authority,
            scope,
            &pending,
        )?;
        if !pending_valid {
            transaction.commit().map_err(repository_unavailable)?;
            authority_guard.finish()?;
            self.lock_snapshots()?.remove(&payload.snapshot_id);
            return Err(MemoryError::new(MemoryErrorCode::CursorExpired));
        }

        let receipt = {
            let mut snapshots = self.lock_snapshots()?;
            let snapshot = snapshots
                .get_mut(&payload.snapshot_id)
                .ok_or_else(|| MemoryError::new(MemoryErrorCode::CursorExpired))?;
            if snapshot.expires_at <= Instant::now() {
                snapshots.remove(&payload.snapshot_id);
                return Err(MemoryError::new(MemoryErrorCode::CursorExpired));
            }
            let stable_position = *snapshot
                .cursor_positions
                .get(&payload.cursor_handle)
                .ok_or_else(|| MemoryError::new(MemoryErrorCode::InvalidCursor))?;
            if stable_position != position {
                return Err(MemoryError::new(MemoryErrorCode::InvalidCursor));
            }
            let (page, next_position) = slice_page(&snapshot.items, position, page_size(params))?;
            let next_cursor = if next_position < snapshot.items.len() {
                let (handle, cursor) =
                    self.build_cursor(&payload.snapshot_id, next_position, &snapshot.binding)?;
                snapshot
                    .cursor_positions
                    .entry(handle)
                    .or_insert(next_position);
                Some(cursor)
            } else {
                None
            };
            MemoryQueryPageReceipt::new(page, next_cursor)
        };
        transaction.commit().map_err(repository_unavailable)?;
        authority_guard.finish()?;
        Ok(receipt)
    }

    /// 在一次独立读快照（WAL 读事务）内物化本次查询的完整排序结果。
    fn build_frozen_items(
        &self,
        request: &MemoryRetrievalRequest,
        normalized: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<FrozenItem>, MemoryError> {
        let scope = request.scope();
        let params = request.params();
        let filters = request.filters();
        let authority = self.repository.deletion_authority();
        // 锁序与 Repository 一致：先 authority 后主库，持有期间不重取。
        let authority_guard = authority.begin_guard()?;
        let mut connection = self.repository.open_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(repository_unavailable)?;
        let items = match &params.memory_id {
            Some(memory_id) if params.include_history => self.history_items(
                &transaction,
                &authority_guard,
                authority,
                scope,
                memory_id,
                filters,
            )?,
            Some(memory_id) => self.direct_items(
                &transaction,
                &authority_guard,
                authority,
                scope,
                memory_id,
                params.as_of.as_deref(),
                filters,
            )?,
            None => self.relevance_items(
                &transaction,
                &authority_guard,
                authority,
                scope,
                normalized,
                params.as_of.as_deref(),
                filters,
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
        filters: &MemoryRetrievalFilters,
        now: DateTime<Utc>,
    ) -> Result<Vec<FrozenItem>, MemoryError> {
        if let Some(as_of) = as_of {
            return self.as_of_relevance_items(
                transaction,
                authority_guard,
                authority,
                scope,
                normalized,
                filters,
                as_of,
            );
        }

        let mut statement = transaction
            .prepare(FTS_CANDIDATE_SQL)
            .map_err(repository_unavailable)?;
        let reference_micros = now.timestamp_micros();
        let query_chars: Vec<char> = normalized.chars().collect();
        let mut items = Vec::new();
        let mut scanned = 0_usize;
        while scanned < MAX_CANDIDATES_SCANNED {
            let batch_size = usize::min(
                FTS_SCAN_BATCH_SIZE,
                MAX_CANDIDATES_SCANNED.saturating_sub(scanned),
            );
            let candidates = statement
                .query_map(
                    params![
                        normalized,
                        scope.persona_id(),
                        batch_size as i64,
                        scanned as i64
                    ],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
                )
                .map_err(repository_unavailable)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(repository_unavailable)?;
            let fetched = candidates.len();
            for (memory_id, bm25) in candidates {
                let memory_id = MemoryId(memory_id);
                let Some(record) =
                    self.repository
                        .load_current_on(transaction, scope, &memory_id)?
                else {
                    continue;
                };
                if !record_matches_filters(&record, filters)
                    || candidate_blocked(
                        authority_guard,
                        authority,
                        scope,
                        &memory_id,
                        &record.current_revision.content,
                    )?
                {
                    continue;
                }
                validate_revision_fields(&record.current_revision)?;
                // 候选正文先通过固定字节/字符上限，再进入 Unicode 规范化与 LCS。
                let content_chars: Vec<char> =
                    normalize_search_text(&record.current_revision.content)
                        .chars()
                        .collect();
                if !passes_hard_relevance(&query_chars, &content_chars) {
                    continue;
                }
                let freshness_micros = rfc3339_micros(&record.entry.freshness_at)?;
                let effective_weight = importance_weight(record.entry.importance)
                    * freshness_decay(reference_micros - freshness_micros);
                items.push(FrozenItem::new(
                    &record.entry,
                    record.current_revision.clone(),
                    effective_weight,
                    bm25,
                    freshness_micros,
                )?);
            }
            scanned += fetched;
            if fetched < batch_size {
                break;
            }
        }
        drop(statement);
        // 排序主键为有效权重；同权重按相关度、更新时间、稳定 ID 保证确定顺序。
        items.sort_by(compare_frozen_items);
        Ok(items)
    }

    /// 时间点相关性不能复用 current FTS；先选中 as_of 可读 revision，再对该正文硬匹配。
    #[allow(clippy::too_many_arguments)]
    fn as_of_relevance_items(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        authority_guard: &CanonicalAuthorityGuard<'_>,
        authority: &SqliteMemoryDeletionAuthority,
        scope: &MemoryPersonaScope,
        normalized: &str,
        filters: &MemoryRetrievalFilters,
        as_of: &str,
    ) -> Result<Vec<FrozenItem>, MemoryError> {
        let at_micros = rfc3339_micros(as_of)?;
        let query_chars: Vec<char> = normalized.chars().collect();
        let mut statement = transaction
            .prepare(AS_OF_CANDIDATE_SQL)
            .map_err(repository_unavailable)?;
        let mut items = Vec::new();
        let mut scanned = 0_usize;
        while scanned < MAX_CANDIDATES_SCANNED {
            let batch_size = usize::min(
                FTS_SCAN_BATCH_SIZE,
                MAX_CANDIDATES_SCANNED.saturating_sub(scanned),
            );
            let memory_ids = statement
                .query_map(
                    params![scope.persona_id(), batch_size as i64, scanned as i64],
                    |row| row.get::<_, String>(0),
                )
                .map_err(repository_unavailable)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(repository_unavailable)?;
            let fetched = memory_ids.len();
            for memory_id in memory_ids {
                let memory_id = MemoryId(memory_id);
                let Some(record) =
                    self.repository
                        .load_current_on(transaction, scope, &memory_id)?
                else {
                    continue;
                };
                if !record_matches_filters(&record, filters) {
                    continue;
                }
                let history =
                    self.repository
                        .revision_history_on(transaction, scope, &memory_id)?;
                let Some(revision) = revision_valid_at(history, at_micros)? else {
                    continue;
                };
                if candidate_blocked(
                    authority_guard,
                    authority,
                    scope,
                    &memory_id,
                    &revision.content,
                )? {
                    continue;
                }
                validate_revision_fields(&revision)?;
                let content_chars: Vec<char> =
                    normalize_search_text(&revision.content).chars().collect();
                let relevance_ratio = hard_relevance_ratio(&query_chars, &content_chars);
                if relevance_ratio < RELEVANCE_MIN_CONTIGUOUS_RATIO {
                    continue;
                }
                let freshness_micros = rfc3339_micros(&record.entry.freshness_at)?;
                let effective_weight = importance_weight(record.entry.importance)
                    * freshness_decay(at_micros - freshness_micros);
                items.push(FrozenItem::new(
                    &record.entry,
                    revision,
                    effective_weight,
                    -relevance_ratio,
                    freshness_micros,
                )?);
            }
            scanned += fetched;
            if fetched < batch_size {
                break;
            }
        }
        drop(statement);
        items.sort_by(compare_frozen_items);
        Ok(items)
    }

    /// 直接读取模式：模型显式指定 memory_id，不做相关性门槛。
    #[allow(clippy::too_many_arguments)]
    fn direct_items(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        authority_guard: &CanonicalAuthorityGuard<'_>,
        authority: &SqliteMemoryDeletionAuthority,
        scope: &MemoryPersonaScope,
        memory_id: &MemoryId,
        as_of: Option<&str>,
        filters: &MemoryRetrievalFilters,
    ) -> Result<Vec<FrozenItem>, MemoryError> {
        let record = self
            .repository
            .load_current_on(transaction, scope, memory_id)?
            .ok_or_else(|| MemoryError::new(MemoryErrorCode::MemoryNotFound))?;
        if !record_matches_filters(&record, filters) {
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
        validate_revision_fields(&revision)?;
        // as_of 必须以该时点实际选中的 revision 正文检查派生删除权威，不能借用
        // current 正文，否则会在历史正文与当前正文不同时漏掉删除阻断或误阻断。
        if candidate_blocked(
            authority_guard,
            authority,
            scope,
            memory_id,
            &revision.content,
        )? {
            return Err(MemoryError::new(MemoryErrorCode::MemoryNotFound));
        }
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
        filters: &MemoryRetrievalFilters,
    ) -> Result<Vec<FrozenItem>, MemoryError> {
        let record = self
            .repository
            .load_current_on(transaction, scope, memory_id)?
            .ok_or_else(|| MemoryError::new(MemoryErrorCode::MemoryNotFound))?;
        if !record_matches_filters(&record, filters)
            || candidate_blocked(
                authority_guard,
                authority,
                scope,
                memory_id,
                &record.current_revision.content,
            )?
        {
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
            validate_revision_fields(&revision)?;
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

    fn pending_items_still_readable(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        authority_guard: &CanonicalAuthorityGuard<'_>,
        authority: &SqliteMemoryDeletionAuthority,
        scope: &MemoryPersonaScope,
        pending: &[FrozenItem],
    ) -> Result<bool, MemoryError> {
        for item in pending {
            let history =
                self.repository
                    .revision_history_on(transaction, scope, &item.memory_id)?;
            let Some(revision) = history
                .iter()
                .find(|revision| revision.revision_id == item.revision_id)
            else {
                return Ok(false);
            };
            if !revision.state.is_model_readable()
                || revision.content != item.content
                || candidate_blocked(
                    authority_guard,
                    authority,
                    scope,
                    &item.memory_id,
                    &item.content,
                )?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn binding_digest(
        &self,
        request: &MemoryRetrievalRequest,
        normalized: &str,
    ) -> Result<[u8; 32], MemoryError> {
        let params = request.params();
        let include_history = [u8::from(params.include_history)];
        let as_of = params
            .as_of
            .as_deref()
            .map(canonical_rfc3339)
            .transpose()?
            .unwrap_or_default();
        let memory_id = params
            .memory_id
            .as_ref()
            .map(|memory_id| memory_id.0.as_str())
            .unwrap_or("");
        let normalized_limit = u64::try_from(page_size(params))
            .map_err(|_| MemoryError::new(MemoryErrorCode::RepositoryUnavailable))?;
        let category = request
            .filters()
            .category()
            .map(category_binding_value)
            .unwrap_or("");
        let importance = request
            .filters()
            .importance()
            .map(importance_binding_value)
            .unwrap_or("");
        Ok(self.repository.deletion_authority().keyed_digest(
            CURSOR_BINDING_DOMAIN,
            &[
                request.scope().persona_id().as_bytes(),
                normalized.as_bytes(),
                &normalized_limit.to_be_bytes(),
                category.as_bytes(),
                importance.as_bytes(),
                &include_history,
                as_of.as_bytes(),
                memory_id.as_bytes(),
                RETRIEVAL_SORT_VERSION,
                request.turn().turn_id().as_bytes(),
                request.turn().turn_nonce().as_bytes(),
            ],
        ))
    }

    fn cursor_mac(&self, snapshot_id: &str, cursor_handle: &str, binding: &[u8; 32]) -> String {
        let digest = self.repository.deletion_authority().keyed_digest(
            CURSOR_HMAC_DOMAIN,
            &[snapshot_id.as_bytes(), cursor_handle.as_bytes(), binding],
        );
        hex_encode(&digest)
    }

    fn build_cursor(
        &self,
        snapshot_id: &str,
        position: usize,
        binding: &[u8; 32],
    ) -> Result<(String, MemoryCursor), MemoryError> {
        let position = u64::try_from(position)
            .map_err(|_| MemoryError::new(MemoryErrorCode::RepositoryUnavailable))?;
        let handle_digest = self.repository.deletion_authority().keyed_digest(
            CURSOR_HANDLE_DOMAIN,
            &[snapshot_id.as_bytes(), &position.to_be_bytes(), binding],
        );
        let cursor_handle = hex_encode(&handle_digest[..8]);
        let mac = self.cursor_mac(snapshot_id, &cursor_handle, binding);
        let cursor = MemoryCursor::from_runtime(format!(
            "{CURSOR_PREFIX}.{snapshot_id}.{cursor_handle}.{mac}"
        ))
        .map_err(|_| MemoryError::new(MemoryErrorCode::RepositoryUnavailable))?;
        Ok((cursor_handle, cursor))
    }

    fn next_snapshot_id(&self, request: &MemoryRetrievalRequest, now: DateTime<Utc>) -> String {
        let sequence = self.snapshot_sequence.fetch_add(1, Ordering::Relaxed);
        let digest = self.repository.deletion_authority().keyed_digest(
            SNAPSHOT_ID_DOMAIN,
            &[
                request.scope().persona_id().as_bytes(),
                request.turn().turn_id().as_bytes(),
                request.turn().turn_nonce().as_bytes(),
                &sequence.to_be_bytes(),
                &now.timestamp_micros().to_be_bytes(),
            ],
        );
        hex_encode(&digest[..8])
    }

    fn cursor_ttl(&self) -> MonotonicDuration {
        MonotonicDuration::from_secs(u64::try_from(self.cursor_ttl_seconds).unwrap_or(0))
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
    expires_at: Instant,
    byte_size: usize,
    owner: SnapshotOwner,
    cursor_positions: BTreeMap<String, usize>,
}

/// 冻结快照中的一条有序结果；排序键在快照创建时固化。
#[derive(Clone)]
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
        validate_revision_fields(&revision)?;
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotOwner {
    persona_id: String,
    turn: MemoryRetrievalTurn,
}

impl SnapshotOwner {
    fn from_request(request: &MemoryRetrievalRequest) -> Self {
        Self {
            persona_id: request.scope().persona_id().to_string(),
            turn: request.turn().clone(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SnapshotBudgetLimits {
    global_count: usize,
    global_bytes: usize,
    persona_count: usize,
    persona_bytes: usize,
    turn_count: usize,
    turn_bytes: usize,
}

impl SnapshotBudgetLimits {
    const fn production() -> Self {
        Self {
            global_count: MAX_FROZEN_SNAPSHOTS,
            global_bytes: MAX_FROZEN_SNAPSHOT_BYTES,
            persona_count: MAX_PERSONA_FROZEN_SNAPSHOTS,
            persona_bytes: MAX_PERSONA_FROZEN_SNAPSHOT_BYTES,
            turn_count: MAX_TURN_FROZEN_SNAPSHOTS,
            turn_bytes: MAX_TURN_FROZEN_SNAPSHOT_BYTES,
        }
    }
}

fn ensure_snapshot_budget(
    snapshots: &BTreeMap<String, FrozenSnapshot>,
    owner: &SnapshotOwner,
    new_bytes: usize,
    limits: SnapshotBudgetLimits,
) -> Result<(), MemoryError> {
    let mut global_bytes = 0_usize;
    let mut persona_count = 0_usize;
    let mut persona_bytes = 0_usize;
    let mut turn_count = 0_usize;
    let mut turn_bytes = 0_usize;
    for snapshot in snapshots.values() {
        global_bytes = global_bytes
            .checked_add(snapshot.byte_size)
            .ok_or_else(query_budget_exceeded)?;
        if snapshot.owner.persona_id == owner.persona_id {
            persona_count += 1;
            persona_bytes = persona_bytes
                .checked_add(snapshot.byte_size)
                .ok_or_else(query_budget_exceeded)?;
        }
        if snapshot.owner == *owner {
            turn_count += 1;
            turn_bytes = turn_bytes
                .checked_add(snapshot.byte_size)
                .ok_or_else(query_budget_exceeded)?;
        }
    }
    let exceeds = snapshots.len().saturating_add(1) > limits.global_count
        || global_bytes.saturating_add(new_bytes) > limits.global_bytes
        || persona_count.saturating_add(1) > limits.persona_count
        || persona_bytes.saturating_add(new_bytes) > limits.persona_bytes
        || turn_count.saturating_add(1) > limits.turn_count
        || turn_bytes.saturating_add(new_bytes) > limits.turn_bytes;
    if exceeds {
        return Err(query_budget_exceeded());
    }
    Ok(())
}

fn frozen_snapshot_bytes(items: &[FrozenItem]) -> Result<usize, MemoryError> {
    items.iter().try_fold(0_usize, |total, item| {
        let item_bytes = serde_json::to_vec(&item.to_query_item())
            .map_err(repository_unavailable)?
            .len();
        total
            .checked_add(item_bytes)
            .ok_or_else(query_budget_exceeded)
    })
}

fn compare_frozen_items(left: &FrozenItem, right: &FrozenItem) -> std::cmp::Ordering {
    right
        .effective_weight
        .total_cmp(&left.effective_weight)
        .then_with(|| left.relevance.total_cmp(&right.relevance))
        .then_with(|| right.freshness_micros.cmp(&left.freshness_micros))
        .then_with(|| left.memory_id.0.cmp(&right.memory_id.0))
}

fn record_matches_filters(
    record: &crate::domain::memory::MemoryRecord,
    filters: &MemoryRetrievalFilters,
) -> bool {
    filters
        .category()
        .is_none_or(|category| record.entry.category == category)
        && filters
            .importance()
            .is_none_or(|importance| record.entry.importance == importance)
}

fn validate_revision_fields(revision: &MemoryRevision) -> Result<(), MemoryError> {
    validate_memory_content(&revision.content)?;
    validate_memory_change_reason(&revision.change_reason)
}

fn canonical_rfc3339(value: &str) -> Result<String, MemoryError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|value| {
            value
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Micros, true)
        })
        .map_err(|_| MemoryError::new(MemoryErrorCode::InvalidRequest))
}

fn category_binding_value(category: MemoryCategory) -> &'static str {
    match category {
        MemoryCategory::UserFact => "user_fact",
        MemoryCategory::UserPreference => "user_preference",
        MemoryCategory::SharedExperience => "shared_experience",
        MemoryCategory::Commitment => "commitment",
        MemoryCategory::StoryState => "story_state",
    }
}

fn importance_binding_value(importance: MemoryImportance) -> &'static str {
    match importance {
        MemoryImportance::Low => "low",
        MemoryImportance::Normal => "normal",
        MemoryImportance::High => "high",
    }
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
    hard_relevance_ratio(query, content) >= RELEVANCE_MIN_CONTIGUOUS_RATIO
}

fn hard_relevance_ratio(query: &[char], content: &[char]) -> f64 {
    if query.is_empty() {
        return 0.0;
    }
    longest_common_substring_len(query, content) as f64 / query.len() as f64
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
    cursor_handle: String,
    mac: String,
}

fn parse_cursor(value: &str) -> Result<ParsedCursor, MemoryError> {
    let invalid = || MemoryError::new(MemoryErrorCode::InvalidCursor);
    let parts: Vec<&str> = value.split('.').collect();
    if parts.len() != 4 || parts[0] != CURSOR_PREFIX {
        return Err(invalid());
    }
    let snapshot_id = parts[1];
    if snapshot_id.len() != 16 || !snapshot_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    let cursor_handle = parts[2];
    if cursor_handle.len() != 16 || !cursor_handle.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    let mac = parts[3];
    if mac.len() != 64 || !mac.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    Ok(ParsedCursor {
        snapshot_id: snapshot_id.to_string(),
        cursor_handle: cursor_handle.to_string(),
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
