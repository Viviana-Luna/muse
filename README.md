# Muse

本地优先的角色扮演 Agent，提供多模型聊天、角色管理、语音输入输出、运行时事件和受控工具能力。

## 产品定位

Muse 面向希望在本机创建、演绎和陪伴角色的用户。角色设定、世界观、场景、演绎风格与角色卡共同决定每轮互动；模型、角色、语音、会话和工具执行统一运行在本地桌面宿主中。

## 当前开发状态

Muse 已进入 `v1.0.0-beta.1` 内部 Beta 打磨阶段。该标签表示当前主链已经形成可持续使用的基线，后续重点转向项目认知、真实缺陷审计、架构收口和体验修正；它不表示已经完成公开发布准备，也不承诺发布日期。

macOS 是当前主要开发、长期自用和真实桌面验证环境，但这不表示 macOS 已达到公开分发质量。当前 Beta 只提供源码与标签基线，不创建 GitHub Release 或公开安装包。Windows 相关条件编译和适配基础仍保留，计划在 macOS 主链稳定、具备固定 Windows 测试主机并重新立项后独立适配；当前不宣称 Windows 已受支持。

## 主要能力

- 模型供应商：聊天只支持 DeepSeek 与火山方舟 Agent Plan。两者共用 OpenAI-compatible Chat Completions 传输层，但能力和验证策略由独立 profile 声明；不把协议兼容等同于对 OpenAI、Azure、Ollama、普通方舟在线推理、Coding Plan 或任意自定义服务的支持。
- DeepSeek：已完成模型目录、流式文本、思考内容、工具续轮、用量与余额闭环，默认推荐 `deepseek-v4-flash` 和 `deepseek-v4-pro`；旧 `deepseek-chat`、`deepseek-reasoner` 仅兼容读取。
- 火山方舟 Agent Plan：固定使用 `https://ark.cn-beijing.volces.com/api/plan/v3`、Bearer 鉴权和套餐模型名；“验证并加载模型”通过低输出上限的 Chat probe 完成，不伪造 `/models`，也不声明普通方舟 Model ID、Endpoint ID 或余额能力。普通响应、流式文本、思考内容、工具续轮与最终 usage 已通过真实 Agent Plan Key 验收。
- 模型目录：每个 Provider 的类型、Base URL、明文 API Key、模型列表和默认参数作为完整 Profile 一起保存在受保护的 `config.toml`；设置页面和手工编辑共享同一事实源。设置中心支持模型新增、编辑与删除；当前对话使用的供应商和模型在聊天页切换，并以双要素展示。
- 角色系统：支持角色设定、角色卡导入导出、启用切换，以及头像、立绘、背景三个独立视觉资源槽。
- 语音能力：TTS 与 ASR 均通过 OpenAI-compatible API 接入，密钥和测试状态彼此隔离。
- 会话与上下文：支持独立会话页、恢复、分叉、重命名、归档、安全导出，以及只读 Context Inspector。
- 运行时事件：支持 SSE 流式输出、工具事件、审批、用户问答、取消和按回合冻结的运行策略快照。
- 工具、Skill 与 MCP：提供统一授权边界、角色级允许策略、受控文件与命令工具、网页搜索、独立 Skill 管理和 MCP 连接管理；聊天输入区可从当前角色的有效目录选择一个 Skill 作为本轮附件，发送时由后端按冻结 revision 真实激活；内置 `skill-creator` 可指导模型通过需审批的 `create_skill` 安全创建用户 Skill，服务端原子发布且从下一 Turn 起生效；Skill 正文采用 `skills/<skill-name>/SKILL.md` 标准目录，启停覆盖写入 `config.toml`，MCP Server、普通参数与明文 API Key 作为同一 Profile 保存在同一配置文件。
- 原生桌面交付：Tauri 从安装包内嵌资源加载 UI，不依赖源码目录、外部 Vite 服务或固定 localhost 页面；官方单实例插件会把重复启动聚焦回既有主窗口。

## 技术栈

- 核心领域：`crates/muse-core` 负责配置、角色、模型、工具、语音和存储能力。
- 运行时：`crates/muse-runtime` 负责状态协调、回合快照和 v3 会话存储。
- 本地 API：`crates/muse-local-api` 负责 Axum 路由、安全边界和运行时装配。
- 前端：React 19、Vite 7、TypeScript、Zustand、Radix UI、react-markdown。
- 桌面：Tauri 2；当前以 macOS WKWebView 桌面开发为主，保留未来 Windows WebView2/NSIS 适配基础。
- 存储：平台应用数据目录中的用户级 `config.toml`、版本化 SQLite、Session JSONL 和文件资源；旧 JSON 只作为一次性迁移来源原样保留。

## 快速开始

### 环境要求

- Rust stable 工具链（需包含 `rustfmt` 与 `clippy`）。
- Node.js 22 与 npm。
- 对应平台的 [Tauri 2 前置依赖](https://v2.tauri.app/start/prerequisites/)。
- 打包安装包时使用 `tauri-cli 2.10.1`。

当前开发技术基线仍保留 macOS 13.1 / Safari 16.2，以及未来 Windows 10 22H2、Windows 11 / WebView2 111 的兼容目标，但这些目标不等于已完成公开支持验收。日常真实桌面工作只以 macOS 为主；Windows 适配整体延后。完整边界见 [WebView 兼容基线](docs/webview-compatibility.md)。

### 安装依赖

```bash
git clone https://github.com/Viviana-Luna/muse.git
cd muse
npm ci
cargo build --workspace --locked
```

### 运行桌面应用

```bash
npm run tauri dev
```

Tauri 开发模式会启动根目录 Vite 前端和 `src-tauri/` 桌面宿主。后端只监听本进程分配的 `127.0.0.1` 随机端口；主窗口通过受控的 `runtime_bootstrap` 命令取得 API 地址、内存 Bearer、协议版本和实例 ID，不需要另行启动浏览器后端或暴露固定端口。

### 构建本地测试包

以下命令只用于开发和内部验证，不构成发布说明。

macOS：

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
npm run tauri build -- --bundles dmg --target universal-apple-darwin
```

Windows（未来适配或 CI 诊断使用，当前不属于支持范围）：

```bash
npm run tauri build -- --bundles nsis
```

Windows 测试包输出到 `target/release/bundle/`；macOS Universal 测试包输出到
`target/universal-apple-darwin/release/bundle/`。当前 macOS 构建不使用 Apple Developer ID 签名或公证，只能作为本地或受控测试产物，不能据此创建公开 Release。未来进入版本阶段时必须重新制定签名、公证、最低系统和分发策略。

## 数据目录

`MUSE_DATA_DIR` 是桌面应用数据目录的最高优先级覆盖项：

```bash
export MUSE_DATA_DIR=/your/local/muse-data
```

未设置时，桌面应用使用当前用户主目录下的 `~/.muse`。用户配置保存在带 `schema_version` 的 `config.toml`；外观、输入、恢复、动效和更新偏好、完整 Provider Profile、MCP Server Profile、Skill 启停覆盖以及 Exa 联网搜索配置都在该文件中。Provider Profile 把供应商、Base URL、明文 API Key、模型列表、能力和默认参数放在一起；MCP Server Profile 把 transport、命令或 URL、普通环境变量或 Header、明文 API Key、超时与工具过滤放在一起；Exa API Key 与搜索后端同样原子保存。文件使用当前用户专属权限、耐久原子写入和外部修改冲突检测，不应上传、共享或提交。Skill 正文及其 `scripts/`、`references/`、`assets/` 保留在 `skills/<skill-name>/`，名称只允许小写字母、数字和单连字符。完整会话以及标题、归档、分叉和 revision 等元数据统一记录为 Session v3 JSONL 事件；`runtime/muse.sqlite` 只保存核心结构化状态和可从 JSONL 重建的会话列表索引，并通过 SQLite Backup API 创建一致性备份。Token usage 与 Context snapshot 等可清理运行日志独立保存在 `logs/runtime-usage.sqlite`，不参与核心库备份或恢复。新版本不读取或生成 `sessions/metadata.json`，也不读取旧 `models/config.json`、`mcp/servers.json` 或系统凭据库。

数据目录由进程排他锁保护；桌面重复启动由官方单实例插件唤醒并聚焦既有窗口。首次启动若 `~/.muse` 为空且启动目录是经过项目标识校验的仓库根目录，Muse 会把旧 `.agent-vp-data` 的角色、会话和普通资源复制到 `~/.muse`，但排除旧模型文件与旧模型/MCP 配置；crate 子目录或任意 CWD 中的同名目录不会被采用，原目录保持不变。详见 [升级与数据迁移](docs/upgrade.md)。

## 旧语音模型文件

当前开发分支不再发现、加载、校验、迁移或删除旧 Whisper 等本地模型文件。升级时这些文件会原样留在用户磁盘；语音识别是否可用只取决于 OpenAI-compatible ASR 配置。

## 项目结构

```text
.
├── src/               # React/Vite 前端源码
├── src-tauri/         # 唯一 Tauri 应用、权限、图标和桌面生命周期
│   └── src/
│       ├── main.rs    # 标准薄入口
│       └── lib.rs     # 窗口、单实例、Bootstrap 与 API 生命周期
├── crates/
│   ├── muse-core/      # 与 Tauri 无关的核心领域层
│   ├── muse-runtime/   # 会话与回合协调层
│   └── muse-local-api/ # Axum 本地 API 适配层
├── docs/              # 公开协议与升级文档
├── package.json       # 前端与 Tauri CLI 命令
├── Cargo.toml         # Rust workspace
└── README.md          # 项目介绍
```

Rust 依赖从应用壳向内单向指向 `src-tauri → muse-local-api → muse-runtime → muse-core`；`muse-local-api` 可以直接复用 `muse-core`，领域 crate 不得反向依赖 Tauri。

## 质量检查

日常开发先运行受影响模块的 focused 测试；合并前运行默认 Rust 门禁和完整前端门禁。需要监听端口、Windows ACL 或真实供应商桩服务时，再显式运行平台专项测试。

```bash
# Rust focused 示例
cargo test -p muse-core model::config --lib --locked
cargo test -p muse-local-api router::tests --lib --locked

# 前端 focused 示例
npx vitest run src/views/settings/SettingsDialog.test.tsx
```

合并前的默认门禁：

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
npm run test
npm run build
```

平台专项测试：

```bash
cargo test --workspace --all-targets --locked --features live-tests
```

## 文档

- [本地 API 协议](docs/local-api.md)：桌面 Bootstrap、Bearer、SSE 和 WebSocket 契约。
- [升级与数据迁移](docs/upgrade.md)：数据目录切换、legacy 复制迁移、会话 v3 和模型引用。
- [参与贡献](.github/CONTRIBUTING.md)：开发环境、质量门禁、CI 触发矩阵和版本发布流程。
- [安全政策](.github/SECURITY.md)：漏洞报告方式、支持范围和敏感信息边界。
- [AGENTS.md](AGENTS.md)：公开代理和贡献者协作约束。
- [CLAUDE.md](CLAUDE.md)：与 `AGENTS.md` 一致的代理约束入口。

## License

本项目按 [GNU Affero General Public License v3.0 (AGPL-3.0)](LICENSE) 授权。你可以在许可证允许的范围内使用、修改和分发本项目，包括商业使用；分发修改版本，或通过网络向用户提供修改版本的功能时，必须履行 AGPL-3.0 对应的完整源代码提供义务。需要在不遵循 AGPL-3.0 的专有产品中使用时，可另行联系著作权人协商其他授权。
