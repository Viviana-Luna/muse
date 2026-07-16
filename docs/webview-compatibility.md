# WebView 兼容基线

Muse 使用 Tauri 2 和系统 WebView，不内置独立浏览器内核。前端构建、安装器与公开支持范围必须遵守同一能力下限。

## 平台范围

| 平台 | 安装下限 | WebView 能力下限 | 安装策略 |
|---|---|---|---|
| macOS | macOS 13.1 | Safari/WKWebView 16.2 | 使用系统 WKWebView |
| Windows | Windows 10 22H2、Windows 11 | WebView2 111 | 使用 Evergreen Runtime；NSIS 缺失或版本过低时联网运行 bootstrapper |

不支持 macOS 13.0 及更早版本、Windows 7/8/8.1、Windows 10 22H2 之前的版本或 Linux。当前安装包不内嵌离线 WebView2 Runtime；系统缺失 WebView2 111 或版本过低时，`downloadBootstrapper` 需要网络。存在明确离线交付需求时必须单独评估，不得静默改为固定运行时。

## 前端能力契约

Vite 的 JavaScript 与 CSS 构建目标均固定为 `safari16.2` 和 `chrome111`。这是 Muse 当前核心样式所使用 `color-mix()` 的最低共同能力线；生产产物契约还要求构建器为 `backdrop-filter` 和立绘 `mask-image` 保留 Safari 16.2 所需的 `-webkit-` 前缀。不得依赖 Vite 默认 target，因为依赖升级可能静默改变默认浏览器集合。

新增 Web API、JavaScript 语法或 CSS 能力前必须确认两个目标均可用。仅用于装饰的能力可以通过 `@supports` 渐进增强；影响文字可读性、控件可达性、状态辨识或核心交互的能力必须提升构建及安装下限，或提供经过最低环境截图验证的稳定回退。

## 验收状态

配置与静态契约只证明构建、安装和文档口径一致，不等同于真实桌面验收。正式发布前仍必须在 macOS 13.1、当前 macOS、Windows 10 22H2 与 Windows 11 上记录系统及 WebView 版本，并检查亮暗主题、系统字体、窄窗口、100%/125%/150%/200% 缩放和关键交互。缺少任一环境的实机证据时，不得宣称跨平台发布验收完成。

Windows 实机可使用仓库内的脱敏证据入口记录安装包、环境和进程状态：

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
