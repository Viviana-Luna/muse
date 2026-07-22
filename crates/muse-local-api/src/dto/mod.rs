//! 网页接口 DTO，按 API 业务域组织并从此处统一导出。

mod assets;
mod chat;
mod common;
mod diagnostics;
mod mcp;
mod models;
mod personas;
mod preferences;
mod runtime;
mod sessions;
mod skills;
mod voice;

pub use assets::*;
pub use chat::*;
pub use common::{ErrorResponse, RevisionQuery, StatusResponse};
pub use diagnostics::*;
pub use mcp::*;
pub use models::*;
pub use personas::*;
pub use preferences::*;
pub use runtime::*;
pub use sessions::*;
pub use skills::{SkillCreateRequest, SkillUpdateRequest};
pub use voice::*;

pub(crate) use common::next_api_request_id;
pub(crate) use preferences::{
    AppearancePreferencesResponse, AppearancePreferencesUpdateRequest, ConfigMutationErrorResponse,
};
