# 设置中心前端边界

更新时间：2026-07-15
编写目的：说明设置中心前端模块拆分、面板职责和与后端配置的边界。
文档类型：模块说明
目的进度：当前有效

设置中心从首页组装层中拆出后，后续设置类能力统一放在 `src/views/settings` 下维护。

## 当前已落地面板

- `chat`：对话模型配置，负责 Provider、模型、API Base、API Key、Max Tokens、Temperature；供应商、明文 Key、模型列表与默认参数统一写入用户级 `config.toml` Provider Profile。
- `tts`：语音合成配置，负责外接语音服务、音色、语速、输出格式和测试播报。
- `speech_recognition`：OpenAI-compatible 语音识别配置，负责服务地址、密钥、模型、语言和返回格式。
- `workspace`：工具工作区策略，负责权限模式、沙箱模式和风险提示。
- `web_search`：Exa 联网搜索后端与密钥草稿。默认使用免费 MCP；切换 API 方案后，密钥按 `keep | replace | delete` 动作保存到系统凭据库。
- `appearance`：客户端外观配置，当前负责动效强度；旧版背景模糊与可见度字段继续由本地 API 兼容读取，但不再出现在产品界面。草稿随“保存全部更改”通过本地 API 原子写入用户级 `config.toml`，不进入模型配置或浏览器持久状态。
- `diagnostics`：系统诊断摘要，负责展示当前端点配置、运行状态和真实连通性检测结果。

## 目录结构

- `SettingsDialog.tsx` 只保留弹窗外壳、左侧菜单、关闭拦截和面板路由。
- `types.ts` 放置设置中心共享类型，例如 `SettingsPanel`、`AppearanceSettings` 和 `SettingsDialogProps`。
- `panels/` 放置各设置面板；新增面板优先在该目录下独立成文件，避免继续膨胀外壳组件。
- 设置草稿按 Models、Workspace、Web Search、Appearance 四个领域独立标记；部分保存失败时只清除成功领域，失败草稿继续保留。

## 后端接口边界

- `/api/diagnostics/connectivity` 只在用户点击诊断刷新时执行轻量连通性检测；设置弹窗打开时不自动联网。
- `/api/preferences/appearance` 读取和更新 `config.toml` 中的外观段；后端保留其他配置、注释、顺序和未知字段，并返回稳定字段诊断。手工编辑造成 revision 变化时，页面保存会收到冲突响应，不能覆盖磁盘新值。
- 模型设置与手工配置共享同一个 `config.toml` revision。模型 API 只返回 `api_key_configured`，不会把明文 Key 或掩码放进响应、SQLite、会话和浏览器持久状态。
- 模型资产 API 已退出产品；旧模型文件只留在用户磁盘，应用不会发现、加载、校验或删除它们。
