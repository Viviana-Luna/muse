[CmdletBinding(DefaultParameterSetName = 'EnvironmentOnly')]
param(
  [Parameter(Mandatory = $true)]
  [ValidatePattern('^[0-9a-fA-F]{40}$')]
  [string]$CommitSha,

  [Parameter(Mandatory = $true)]
  [ValidateScript({ Test-Path -LiteralPath $_ -PathType Leaf })]
  [string]$InstallerPath,

  [Parameter(Mandatory = $true, ParameterSetName = 'Running')]
  [ValidateRange(1, [int]::MaxValue)]
  [int]$MainProcessId,

  [Parameter(Mandatory = $true, ParameterSetName = 'Stopped')]
  [switch]$ExpectNoMuseProcess
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
  throw 'Windows 实机证据采集脚本只能在 Windows 上运行。'
}

function Get-RegistryValue {
  param(
    [Parameter(Mandatory = $true)]
    [string[]]$Paths,

    [Parameter(Mandatory = $true)]
    [string]$Name
  )

  foreach ($path in $Paths) {
    if (-not (Test-Path -LiteralPath $path)) {
      continue
    }
    $item = Get-ItemProperty -LiteralPath $path -Name $Name -ErrorAction SilentlyContinue
    if ($null -eq $item) {
      continue
    }
    $value = $item.$Name
    if ($null -ne $value -and -not [string]::IsNullOrWhiteSpace([string]$value)) {
      return [string]$value
    }
  }
  return $null
}

function Convert-ThemeValue {
  param([AllowNull()][string]$Value)

  if ($Value -eq '0') {
    return 'dark'
  }
  if ($Value -eq '1') {
    return 'light'
  }
  return 'unknown'
}

function Get-DescendantProcessCount {
  param(
    [Parameter(Mandatory = $true)]
    [int]$RootProcessId
  )

  $all = @(Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId)
  $queue = [Collections.Generic.Queue[int]]::new()
  $visited = [Collections.Generic.HashSet[int]]::new()
  $queue.Enqueue($RootProcessId)
  [void]$visited.Add($RootProcessId)
  $count = 0
  while ($queue.Count -gt 0) {
    $parent = $queue.Dequeue()
    foreach ($child in $all | Where-Object { $_.ParentProcessId -eq $parent }) {
      $childProcessId = [int]$child.ProcessId
      if ($visited.Add($childProcessId)) {
        $count += 1
        $queue.Enqueue($childProcessId)
      }
    }
  }
  return $count
}

$operatingSystem = Get-CimInstance Win32_OperatingSystem
if ([int]$operatingSystem.ProductType -ne 1) {
  throw '当前系统不是 Windows 客户端工作站。'
}
$windowsBuild = [int]$operatingSystem.BuildNumber
$windowsTarget = if ($windowsBuild -eq 19045) {
  'windows_10_22h2'
} elseif ($windowsBuild -ge 22000) {
  'windows_11'
} else {
  throw "当前 Windows build $windowsBuild 不在支持的实机矩阵内。"
}

# Microsoft WebView2 官方分发文档指定该 Client ID 用于查询 Evergreen Runtime。
$webView2ClientId = '{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
$webView2Version = Get-RegistryValue -Paths @(
  "Registry::HKEY_CURRENT_USER\Software\Microsoft\EdgeUpdate\Clients\$webView2ClientId",
  "Registry::HKEY_LOCAL_MACHINE\Software\Microsoft\EdgeUpdate\Clients\$webView2ClientId",
  "Registry::HKEY_LOCAL_MACHINE\Software\WOW6432Node\Microsoft\EdgeUpdate\Clients\$webView2ClientId"
) -Name 'pv'
if ($null -eq $webView2Version) {
  throw '未找到 Evergreen WebView2 Runtime 版本。'
}
if ([version]$webView2Version -lt [version]'111.0.0.0') {
  throw "Evergreen WebView2 Runtime 低于最低支持版本 111：$webView2Version"
}

$desktopPaths = @('Registry::HKEY_CURRENT_USER\Control Panel\Desktop')
$personalizePaths = @(
  'Registry::HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize'
)
$appliedDpiValue = Get-RegistryValue -Paths $desktopPaths -Name 'LogPixels'
if ($null -eq $appliedDpiValue) {
  $displayScalePercent = $null
} else {
  $appliedDpi = [int]$appliedDpiValue
  $displayScalePercent = [int][Math]::Round(($appliedDpi / 96.0) * 100)
}

$appsUseLightTheme = Get-RegistryValue -Paths $personalizePaths -Name 'AppsUseLightTheme'
$systemUsesLightTheme = Get-RegistryValue -Paths $personalizePaths -Name 'SystemUsesLightTheme'
$applicationTheme = Convert-ThemeValue -Value $appsUseLightTheme
$systemTheme = Convert-ThemeValue -Value $systemUsesLightTheme

$processEvidence = [ordered]@{
  expectation = 'not_checked'
  muse_process_count = $null
  main_process_present = $null
  descendant_process_count = $null
  main_window_scale_percent = $null
}

if ($PSCmdlet.ParameterSetName -eq 'Running') {
  $mainProcess = Get-Process -Id $MainProcessId -ErrorAction Stop
  if ($mainProcess.ProcessName -ne 'muse') {
    throw '指定的主进程不是 Muse。'
  }
  if ($mainProcess.MainWindowHandle -eq [IntPtr]::Zero) {
    throw 'Muse 主进程尚未暴露可验收的桌面窗口。'
  }
  if ($null -eq ('MuseDpiEvidence' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class MuseDpiEvidence {
    [DllImport("user32.dll")]
    public static extern uint GetDpiForWindow(IntPtr windowHandle);
}
'@
  }
  $mainWindowDpi = [MuseDpiEvidence]::GetDpiForWindow($mainProcess.MainWindowHandle)
  if ($mainWindowDpi -le 0) {
    throw '无法读取 Muse 主窗口 DPI。'
  }
  $processEvidence.expectation = 'running'
  $processEvidence.muse_process_count = @(Get-Process -Name muse -ErrorAction SilentlyContinue).Count
  $processEvidence.main_process_present = $true
  $processEvidence.descendant_process_count = Get-DescendantProcessCount -RootProcessId $MainProcessId
  $processEvidence.main_window_scale_percent = [int][Math]::Round(($mainWindowDpi / 96.0) * 100)
} elseif ($PSCmdlet.ParameterSetName -eq 'Stopped') {
  $remainingMuseProcesses = @(Get-Process -Name muse -ErrorAction SilentlyContinue)
  if ($remainingMuseProcesses.Count -ne 0) {
    throw 'Muse 退出后仍存在残留进程。'
  }
  $processEvidence.expectation = 'stopped'
  $processEvidence.muse_process_count = 0
  $processEvidence.main_process_present = $false
  $processEvidence.descendant_process_count = 0
}

$installer = Get-Item -LiteralPath $InstallerPath
$installerHash = Get-FileHash -LiteralPath $installer.FullName -Algorithm SHA256

[ordered]@{
  schema_version = 'muse-windows-acceptance-evidence/v1'
  collected_at_utc = [DateTime]::UtcNow.ToString('o')
  commit_sha = $CommitSha.ToLowerInvariant()
  installer = [ordered]@{
    size_bytes = $installer.Length
    sha256 = $installerHash.Hash.ToLowerInvariant()
  }
  operating_system = [ordered]@{
    target = $windowsTarget
    caption = [string]$operatingSystem.Caption
    version = [string]$operatingSystem.Version
    build_number = [string]$operatingSystem.BuildNumber
    architecture = [string]$operatingSystem.OSArchitecture
  }
  webview2_runtime_version = $webView2Version
  display = [ordered]@{
    configured_system_scale_percent = $displayScalePercent
    application_theme = $applicationTheme
    system_theme = $systemTheme
  }
  process = $processEvidence
} | ConvertTo-Json -Depth 5
