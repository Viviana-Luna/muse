//! Persona 长期记忆的 SQLite 存储协调入口。

mod authority;
mod recovery;
mod repository;
mod retriever;

pub use authority::SqliteMemoryDeletionAuthority;
pub use repository::{
    MAX_REVISION_HISTORY_PAGE_SIZE, MemoryRevisionHistoryPage, SqliteMemoryRepository,
    normalize_memory_fts_query,
};
pub use retriever::{
    DEFAULT_MEMORY_QUERY_PAGE_SIZE, MAX_MEMORY_QUERY_PAGE_SIZE, MEMORY_QUERY_PAGE_TOKEN_BUDGET,
    MEMORY_QUERY_TURN_MAX_CALLS, MEMORY_QUERY_TURN_TOKEN_BUDGET, MemoryQueryBudget,
    SqliteMemoryRetriever, estimated_memory_query_page_tokens,
};

pub(crate) use authority::{
    AuthorityAnchor, open_authority_for_anchor, prepare_authority_for_migration,
};
pub(crate) use recovery::reconcile_runtime_memory;

#[cfg(test)]
mod tests;
