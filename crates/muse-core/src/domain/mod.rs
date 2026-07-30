//! 运行底座核心层。
//!
//! 这里承载智能体运行底座的一等能力边界：角色上下文、工具、MCP、技能、
//! 运行协议与单轮上下文。角色、MCP resource 和 skill 都属于模型可感知的上下文源，
//! 因此收口到同一个 core 边界下。

pub mod conversation;
pub mod mcp;
pub mod memory;
pub mod persona;
pub mod protocol;
pub mod runtime;
pub mod skill;
pub mod tool;
pub mod turn;
pub mod usage;
