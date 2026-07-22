//! 多个 API 域共享的轻量查询参数。

use serde::Deserialize;

/// 使用 revision 的删除请求。
#[derive(Deserialize)]
pub struct RevisionQuery {
    pub revision: String,
}
