//! 运行模式模块，定义默认工作态与可选计划工具预设；旧日常值仅用于历史兼容。

use serde::{Deserialize, Serialize};

/// 运行时模式协议。新状态固定使用 `Focus`，`Daily` 只读取旧记录。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    /// 仅用于读取旧协议；新运行时不再进入日常模式。
    Daily,
    #[default]
    Focus,
}

impl RuntimeMode {
    /// 返回协议和前端展示使用的稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Focus => "focus",
        }
    }
}

/// 专注模式下的工具档位。
///
/// `Plan` 是按任务需要启用的只读计划档位，不是进入专注模式后的必经阶段。
/// `Build` 表示默认专注工作档位，允许执行已获授权的编辑、命令和 MCP 工具。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FocusPhase {
    Plan,
    #[default]
    Build,
}

impl FocusPhase {
    /// 返回协议和前端展示使用的稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Build => "build",
        }
    }
}

/// 实际用于过滤工具池的预设。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPreset {
    Daily,
    FocusPlan,
    FocusBuild,
}

impl ToolPreset {
    /// 返回协议和前端展示使用的稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::FocusPlan => "focus_plan",
            Self::FocusBuild => "focus_build",
        }
    }

    /// 从协议字符串恢复工具预设。
    pub fn from_protocol(value: &str) -> Option<Self> {
        match value {
            "daily" => Some(Self::Daily),
            "focus_plan" => Some(Self::FocusPlan),
            "focus_build" => Some(Self::FocusBuild),
            _ => None,
        }
    }
}

/// 当前运行模式快照。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RuntimeModeState {
    pub mode: RuntimeMode,
    pub focus_phase: FocusPhase,
}

impl RuntimeModeState {
    /// 根据当前模式和阶段计算实际工具预设。
    pub fn tool_preset(self) -> ToolPreset {
        match (self.mode, self.focus_phase) {
            (RuntimeMode::Daily, _) => ToolPreset::Daily,
            (RuntimeMode::Focus, FocusPhase::Plan) => ToolPreset::FocusPlan,
            (RuntimeMode::Focus, FocusPhase::Build) => ToolPreset::FocusBuild,
        }
    }

    /// 旧调用兼容别名；日常模式已移除，统一回到默认工作态。
    pub fn daily() -> Self {
        Self::focus_build()
    }

    /// 进入默认专注工作档位。
    pub fn focus() -> Self {
        Self::focus_build()
    }

    /// 进入专注计划档位。
    pub fn focus_plan() -> Self {
        Self {
            mode: RuntimeMode::Focus,
            focus_phase: FocusPhase::Plan,
        }
    }

    /// 进入专注工作档位。
    pub fn focus_build() -> Self {
        Self {
            mode: RuntimeMode::Focus,
            focus_phase: FocusPhase::Build,
        }
    }
}

/// 当前专注任务清单条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeTodoItem {
    pub id: String,
    pub content: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{FocusPhase, RuntimeMode, RuntimeModeState, ToolPreset};

    // 验证专注模式默认进入可执行工作预设，而不是强制计划预设。
    #[test]
    fn focus_defaults_to_work_preset() {
        let state = RuntimeModeState::focus();

        assert_eq!(state.mode, RuntimeMode::Focus);
        assert_eq!(state.focus_phase, FocusPhase::Build);
        assert_eq!(state.tool_preset(), ToolPreset::FocusBuild);
    }

    #[test]
    fn runtime_defaults_to_work_preset() {
        assert_eq!(RuntimeModeState::default(), RuntimeModeState::focus_build());
    }

    // 验证计划预设是专注模式下的显式可选档位。
    #[test]
    fn focus_plan_is_explicit_optional_preset() {
        let state = RuntimeModeState::focus_plan();

        assert_eq!(state.mode, RuntimeMode::Focus);
        assert_eq!(state.focus_phase, FocusPhase::Plan);
        assert_eq!(state.tool_preset(), ToolPreset::FocusPlan);
    }
}
