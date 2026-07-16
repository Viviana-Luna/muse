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
- 桌面目标环境为 **macOS 13.1+** 或 **Windows 10 22H2 / Windows 11**；Windows 使用 **WebView2 111+** 的 Evergreen Runtime。修改前端能力、构建目标或安装配置时必须同步维护 [`docs/webview-compatibility.md`](../docs/webview-compatibility.md) 与 CI 契约。

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

`.github/workflows/ci.yml` 是实际执行契约。触发矩阵固定如下：

| 事件 | quality | macOS DMG | Windows NSIS | GitHub Release |
|---|---:|---:|---:|---:|
| 面向 `master` 的 Pull Request | 是 | 否 | 否 | 否 |
| `master` push | 是 | 否 | 否 | 否 |
| 手动 `workflow_dispatch` | 是 | 否 | 否 | 否 |
| 当前仓库的 `v*` 标签 | 是 | 是 | 是 | 是 |

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

维护者发布版本时还必须遵守以下规则：

1. 先确认 `master` quality 全绿，并核对桌面、前端和全部 Rust crate 的版本一致。
2. 确认待标记提交已经属于 `master`，再创建新的不可变 `v*` 标签；不得从 `dev`、临时分支或未合入 `master` 的提交创建正式版本。
3. 等待 macOS/Windows 安装包校验、任意 CWD 启动、单实例和退出清理冒烟全部通过。
4. Release 必须包含唯一 DMG、唯一 NSIS 和 `SHA256SUMS`，发布后下载复核校验和。

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
6. **测试与提升**：改动进入 `dev` 后，由维护者构建短期测试版本并完成真实环境验收；达到验收口径后，才将 `dev` 提升到 `master` 并从 `master` 创建正式版本。
