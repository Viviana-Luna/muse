//! Persona 长期记忆的 SQLite 存储协调入口。

mod authority;
mod recovery;
mod repository;

pub use authority::SqliteMemoryDeletionAuthority;
pub use repository::{SqliteMemoryRepository, normalize_memory_fts_query};

pub(crate) use authority::{
    AuthorityAnchor, open_authority_for_anchor, prepare_authority_for_migration,
};
pub(crate) use recovery::reconcile_runtime_memory;

#[cfg(test)]
mod tests;
