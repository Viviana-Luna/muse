//! 网页接口处理器兼容 façade。
//!
//! Router 只依赖本模块稳定导出的 handler 名称；具体 API 适配与工具实现放在内部模块，
//! 避免再次把运行时、模型、角色和工具职责堆叠到单一源文件。

mod implementation;
mod runtime_api;
mod skills;

pub(crate) use implementation::*;
pub(crate) use runtime_api::{handle_runtime_mode, handle_runtime_todos};
pub(crate) use skills::{
    handle_create_skill, handle_delete_skill, handle_get_skill, handle_skills, handle_update_skill,
};
