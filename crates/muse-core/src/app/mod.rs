//! 应用级配置与存储入口模块，承接跨领域共享但不属于 harness core 的基础能力。

pub mod config;
pub mod log_storage;
pub mod memory_storage;
pub mod preferences;
pub mod secret;
pub mod storage;
#[cfg(windows)]
pub(crate) mod windows_acl;
