# WebView 兼容基线

Muse 使用 Tauri 2 和系统 WebView，不内置独立浏览器内核。本文区分当前开发环境、未来兼容目标和公开支持范围，避免把静态构建目标误写成已完成平台支持。

Muse 当前仍处于功能与架构开发阶段，没有 Alpha、Beta、RC 或正式发布计划。macOS 是主要开发和真实桌面验证环境；Windows 适配整体延后。

## 平台范围

| 平台 | 技术目标 | WebView 能力目标 | 当前状态 |
|---|---|---|---|
| macOS | macOS 13.1 | Safari/WKWebView 16.2 | 当前主要开发环境；最低系统与发布支持尚未最终验收 |
| Windows | Windows 10 22H2、Windows 11 | WebView2 111 | 未来适配目标；当前不属于已支持或发布平台 |

上述版本是代码与构建配置保留的技术目标，不是当前安装包支持声明。未来恢复 Windows 适配时，仍按 Evergreen Runtime 和 `downloadBootstrapper` 方案验证 WebView2 111；Windows 7/8/8.1、Windows 10 22H2 之前版本和 Linux 不在计划范围。正式支持范围必须在以后版本阶段单独确认。

## 前端能力契约

Vite 的 JavaScript 与 CSS 构建目标均固定为 `safari16.2` 和 `chrome111`。这是 Muse 当前核心样式所使用 `color-mix()` 的最低共同能力线；生产产物契约还要求构建器为 `backdrop-filter` 和立绘 `mask-image` 保留 Safari 16.2 所需的 `-webkit-` 前缀。不得依赖 Vite 默认 target，因为依赖升级可能静默改变默认浏览器集合。

新增 Web API、JavaScript 语法或 CSS 能力前必须优先确认 Safari 16.2 目标；同时保留 Chrome 111 构建兼容可以减少未来 Windows 恢复成本，但不能替代 WebView2 实机验证。仅用于装饰的能力可以通过 `@supports` 渐进增强；影响文字可读性、控件可达性、状态辨识或核心交互的能力必须提供稳定回退。

## 验收状态

配置与静态契约只证明构建边界，没有任何发布含义。当前只在可持续使用的 macOS 环境持续检查 WKWebView、亮暗主题、系统字体、窄窗口、麦克风权限、缩放和关键交互；这些结果仍只属于开发验收。

Windows 10 22H2、Windows 11、WebView2、100%/125%/150%/200% 缩放、ACL、安装器和进程生命周期等证据，等待以后具备固定 Windows 主机并重新立项后执行。当前缺少这些结果不会阻塞 macOS 开发，但也禁止宣称跨平台支持。

未来 Windows 实机专项可使用仓库内的脱敏证据入口记录安装包、环境和进程状态：

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

脚本会拒绝支持矩阵之外的 Windows build 或低于 111 的 WebView2 Runtime，并输出提交 SHA、安装包 SHA-256、系统版本、主窗口实际缩放比、主题和进程计数。输出不包含用户名、机器名、进程命令行或 Muse 配置内容。该 JSON 只是可复核的环境证据，不代替亮暗主题、窄窗口和关键交互的人工实机检查。
