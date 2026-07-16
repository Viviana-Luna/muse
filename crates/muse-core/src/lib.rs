//! Muse 领域核心模块，提供配置、角色、工具、模型和语音基础能力。

pub mod app;
pub mod domain;
pub mod model;
pub mod process_supervision;
pub mod speech;

pub use app::{config, storage};
