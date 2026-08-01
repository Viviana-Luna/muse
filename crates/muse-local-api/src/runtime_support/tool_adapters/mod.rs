//! 运行时工具的本地平台适配。

use super::*;

mod capabilities;
pub(in crate::runtime_support) use capabilities::*;
pub(in crate::runtime_support) mod command_environment;
pub(in crate::runtime_support) mod result_archive;

mod approval_review;
pub(in crate::runtime_support) use approval_review::*;
mod approval_flow;
pub(in crate::runtime_support) use approval_flow::*;
mod registry;
pub(in crate::runtime_support) use registry::*;
mod interaction;
pub(in crate::runtime_support) use interaction::*;
mod files;
pub(in crate::runtime_support) use files::*;
mod command;
pub(in crate::runtime_support) use command::*;
mod network;
pub(in crate::runtime_support) use network::*;
#[path = "mcp.rs"]
mod mcp_adapter;
pub(in crate::runtime_support) use mcp_adapter::*;
mod session;
pub(in crate::runtime_support) use session::*;
mod memory;
pub(in crate::runtime_support) use memory::*;
mod persona_context;
pub(in crate::runtime_support) use persona_context::*;
