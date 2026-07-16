//! 旧 `mcp/servers.json` 的只读迁移结构。
//!
//! 运行时和管理 API 不得通过本模块写入旧 JSON；迁移成功后该文件只作为原样保留的历史来源。

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct PersistedMcpStore {
    pub schema_version: u32,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub servers: BTreeMap<String, Value>,
}

pub(crate) fn legacy_mcp_store_path(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join("mcp").join("servers.json")
}
