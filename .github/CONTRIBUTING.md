# Muse 参与贡献指南

首先，感谢你对 **Muse** 的关注与支持！我们欢迎社区开发者参与贡献代码、修复问题、改善文档和提出建设性建议。

在参与项目之前，请务必仔细阅读本指南，以确保协作流程顺畅高效，并且符合项目的架构原则与开源协议约束。

---

## ⚖️ 开源协议须知

本项目采用 **[GNU Affero General Public License v3.0 (AGPL-3.0)](../LICENSE)** 协议开源：

1. **贡献授权**：提交 Pull Request 代表你有权提供相关代码，并同意贡献按本项目的 **AGPL-3.0** 许可证分发。
2. **网络使用义务**：分发修改版本，或通过网络向用户提供修改版本的功能时，应按 AGPL-3.0 提供对应完整源代码。AGPL-3.0 本身不禁止商业使用；需要专有授权时应另行与著作权人协商。

---

## 🛠️ 开发环境准备

### 前置依赖
- **Rust stable** 工具链，并安装 `rustfmt` 与 `clippy`（用于后端服务与本地工具运行时编译）。
- **Node.js 22** 与 **npm**（用于前端 UI 开发与构建）。
- 对应平台的 **Tauri 2 前置依赖**（用于原生桌面构建与调试）。
- 当前主要开发和真实桌面验证环境为 **macOS**；代码仍保留 **macOS 13.1 / Safari 16.2** 技术基线。**Windows 10 22H2 / Windows 11、WebView2 111+** 是未来适配目标，当前不属于已支持平台。修改前端能力、构建目标或安装配置时必须同步维护 [`docs/webview-compatibility.md`](../docs/webview-compatibility.md) 与 CI 契约。

### 仓库克隆与依赖安装

```bash
git clone https://github.com/Viviana-Luna/muse.git
cd muse

# 编译 Rust 后端工作空间
cargo build

# 安装前端与 Tauri CLI 依赖
npm ci
```

---

## 🚀 启动本地联调环境

Muse 使用 Tauri 标准的根前端加 `src-tauri/` 结构。开发模式由 Tauri 同时管理 Vite、桌面窗口和随机回环 API：

```bash
npm run tauri dev
```

未设置 `MUSE_DATA_DIR` 时，桌面使用当前用户主目录下的 `~/.muse`；不要假定数据写入仓库。需要隔离调试数据时应显式设置临时 `MUSE_DATA_DIR`。

只做无后端的前端样式调试时可以单独启动 Vite，但该页面不能替代 Tauri Bootstrap 和真实桌面验收：

```bash
npm run dev
```

### 验证生产内嵌资源入口

```bash
npm run build
cargo run -p muse
```

桌面窗口从构建资源加载 `index.html`，不依赖 Vite、固定端口或源码目录。

### 4. 清理构建产物

Rust `target/` 超过 20 GiB，或完成一次完整发布验证后，运行：

```bash
bash scripts/clean-build-artifacts.sh
```

该脚本只清理仓库内固定的 Rust/前端可再生成目录，保留 `node_modules`、本地模型、会话、迁移备份和运行配置。脚本不接受自定义路径参数，遇到符号链接构建目录会拒绝执行。

---

## 📐 开发架构原则与提交规范

提交代码前，请确保遵循以下项目的开发约束（完整规范请见项目根目录下的 [`AGENTS.md`](../AGENTS.md)）：

### 1. 代码质量与重用
- **优先复用**：新增功能优先复用 `crates/muse-core`、`crates/muse-runtime`、`crates/muse-local-api` 和现有 helper，避免重复造轮子。`src-tauri` 只承载桌面外壳；新增抽象必须能切实降低耦合或真实复杂度。
- **注释精简**：代码注释仅用于解释复杂的边界条件、特殊架构设计原因或风险点，不写空泛、显而易见的废话注释。
- **公开术语约定**：代码标识符、接口路径、依赖包名、配置键、协议字段和第三方专有名称按既有英文命名规范保留。

### 2. 安全与去敏红线（P0）
绝对严禁在提交的代码中包含以下任何信息：
- ❌ **敏感凭证**：API Keys、Secret Tokens 或登录密钥。
- ❌ **硬编码绝对路径**：例如写死的本机特定目录路径（`/Users/xxx/...`）。
- ❌ **私有模型与二进制资产**：不要提交大体积的音频、语音或模型文件。
- ❌ **运行日志与本地配置**：不可提交 `.muse/`、legacy `.agent-vp-data/` 或个人私有上下文。

## 🔁 CI 与发布流程

Muse 当前处于 `v1.0.0-beta.1` 内部 Beta 打磨阶段。带 `-beta.N` 后缀的标签只标记 `dev` 上经过确认的可用基线并运行 quality，不创建安装包或 GitHub Release；稳定版标签、公开安装包和正式 Release 仍需以后从 `master` 单独启动发布评估。任何版本阶段都不得以“准备发布”为理由降低质量门禁。

`.github/workflows/ci.yml` 是自动化实际执行契约。当前日常协作只把 quality 作为有效门禁：

| 事件 | quality | 测试安装包 | GitHub Release |
|---|---:|---:|---:|
| 面向 `dev` 或 `master` 的 Pull Request | 是 | 否 | 否 |
| `dev` 或 `master` push | 是 | 否 | 否 |
| 普通手动 `workflow_dispatch` | 是 | 默认不构建 | 否 |
| `vX.Y.Z-beta.N` 预发布标签 | 是 | 否 | 否 |
| 不含预发布后缀的稳定版标签 | 是 | 未来发布评估决定 | 未来发布评估决定 |

工作流中的双平台打包路径不会由 Beta 标签触发，也不是当前支持或发布授权。未来只有维护者明确启动正式发布评估后，才能根据当时批准的平台范围重写并启用安装包、签名、公证、Release asset 和校验和契约。

贡献者开发时先运行受影响模块的 focused 测试；提交前运行默认门禁：

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
npm run test
npm run build
```

前端只运行行为测试时使用 `npm run test:frontend`。Keychain、本地监听端口和供应商桩服务属于平台专项测试，使用：

```bash
cargo test --workspace --all-targets --locked --features live-tests
```

当前平台策略：macOS 是主要开发、长期自用和真实桌面验证环境，但并未达到发布质量；Windows 条件编译和适配基础保留，等待以后有固定 Windows 主机时重新立项。自动化 Windows 结果不能代替 Windows 实机，也不阻塞当前 macOS 开发。

未来若维护者明确启动版本阶段评估，必须新建独立计划，重新确认目标平台、版本一致性、最低系统、签名、公证、安装升级、资产数量和 `SHA256SUMS`，不能沿用当前遗留双平台发布假设。

修改 Actions 时必须使用完整提交 SHA，不得改回浮动标签。默认 workflow 权限保持只读，只有 Release job 可以请求 `contents: write`。Pull Request 不得接触真实供应商密钥；测试使用 mock、fixture 或显式测试值。

CI 失败时先查看对应 job 日志并修复根因，不通过删除测试、降低安全断言或重复无诊断 rerun 绕过门禁。

---

## 🔄 Pull Request 流程

Muse 使用 `master`、`dev` 和临时功能分支三级流转：`master` 只承载正式发布，`dev` 承载日常集成和测试版本，功能分支只承担单一临时任务。普通贡献不得直接进入 `master`。

1. **Fork 仓库**并从最新 `dev` 拉取临时开发分支：
   ```bash
   git switch dev
   git pull --ff-only
   git checkout -b feat/your-feature-name
   ```
2. **开发与测试**：先运行改动范围内的 focused 测试；收口时运行 `cargo fmt --all --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、Rust 默认门禁、`npm run test` 和 `npm run build`。
3. **收口分支**：提交前把临时分支 rebase 到最新 `dev`；多个相互依赖的临时分支先按依赖顺序收口到一个临时集成分支，并在 rebase 后重新运行质量门禁。
4. **提交 Commit**：Commit 信息保持清晰简明，推荐使用 conventional commits 规范（如 `feat: add quick brief tool`，`fix: resolve UI overlay issue on mobile`）。
5. **发起 PR 到 `dev`**：在 PR 描述中写明改动动机、实现思路和验证结果。功能分支不得直接向 `master` 发起常规合并。
6. **测试与提升**：改动进入 `dev` 后，由维护者在 macOS 主环境持续真实使用和验证；是否进入版本阶段必须以后单独决策，不能因一次门禁或测试包通过自动提升到 `master` 或创建正式版本。
