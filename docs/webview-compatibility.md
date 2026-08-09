# WebView 兼容基线

Muse 使用 Tauri 2 和系统 WebView，不内置独立浏览器内核。本文区分开发环境、源码支持范围和公开分发状态，避免把构建目标或内部安装包误写成公开发行物。

Muse 当前工程版本为 `v1.0.0-beta.2`。Windows 支持声明只有在该源码标签指向的同一提交和同一 NSIS 测试包完成全部自动化与实机验收后才生效；Beta 标签不附带公开安装包，也不创建 GitHub Release。macOS 仍是主要开发和真实桌面验证环境。

## 平台范围

| 平台 | 技术目标 | WebView 能力目标 | 当前状态 |
|---|---|---|---|
| macOS | macOS 13.1 | Safari/WKWebView 16.2 | 当前主要开发环境；最低系统与公开分发支持尚未最终验收 |
| Windows | Windows 11 25H2 x64（build 26200） | Evergreen WebView2 111+ | 从 `v1.0.0-beta.2` 源码标签起支持；当前无公开安装包 |

Windows 支持仅覆盖 25H2 x64，固定实机为 Windows 11 Pro、build 26200；安装器继续使用 Evergreen Runtime 与 `downloadBootstrapper`，最低 WebView2 为 111。Windows 10、Windows 11 24H2/26H1、ARM64、Windows Server、Windows 7/8/8.1 和 Linux 不在支持范围。源码支持不等于提供公开、签名或自动更新的 NSIS。

Windows 宿主同时开启窗口与 WebView 透明能力，但默认 `background_opacity = 1.0`，视觉仍为实色。用户显式选择半透明背景后，只有应用画布透出桌面，导航、内容和标题栏控件浮岛继续使用独立玻璃表面；该模式必须与实色模式一并完成 DPI、亮暗主题、最大化和可读性验收。

## 前端能力契约

Vite 的 JavaScript 与 CSS 构建目标均固定为 `safari16.2` 和 `chrome111`。这是 Muse 当前核心样式所使用 `color-mix()` 的最低共同能力线；生产产物契约还要求构建器为 `backdrop-filter` 和立绘 `mask-image` 保留 Safari 16.2 所需的 `-webkit-` 前缀。不得依赖 Vite 默认 target，因为依赖升级可能静默改变默认浏览器集合。

新增 Web API、JavaScript 语法或 CSS 能力前必须同时确认 Safari 16.2 与 Chrome/WebView2 111 目标。静态构建兼容不能替代 Windows 11 25H2 实机验证；仅用于装饰的能力可以通过 `@supports` 渐进增强，影响文字可读性、控件可达性、状态辨识或核心交互的能力必须提供稳定回退。

## 验收状态

配置与静态契约只证明构建边界。Windows 11 支持基线还必须在固定 25H2 x64 主机完成 WebView2、100%/125%/150%/200% 缩放、ACL、安装升级、进程生命周期和产品主链验收；任何证据缺失时不得创建支持标签。

Windows 实机专项使用仓库内的脱敏证据入口记录安装包、环境和进程状态：

```powershell
pwsh -File scripts/collect-windows-acceptance-evidence.ps1 `
  -CommitSha <40 位提交 SHA> `
  -InstallerPath <NSIS 安装包路径> `
  -MainProcessId <Muse PID>

pwsh -File scripts/collect-windows-acceptance-evidence.ps1 `
  -CommitSha <40 位提交 SHA> `
  -InstallerPath <NSIS 安装包路径> `
  -ExpectNoMuseProcess
```

脚本输出 `muse-windows-acceptance-evidence/v2`，只接受 Windows 11 25H2 build 26200、x64 和 WebView2 111+，并记录提交 SHA、安装包 SHA-256、系统版本、主窗口实际缩放比、主题和进程计数。输出不包含用户名、机器名、进程命令行或 Muse 配置内容。该 JSON 只是可复核的环境证据，不代替亮暗主题、窄窗口、标题栏和关键交互的人工实机检查。
