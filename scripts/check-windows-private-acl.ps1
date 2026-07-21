[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$DataDir
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not $IsWindows) {
  throw 'Windows ACL 验收脚本只能在 Windows 上运行。'
}

$resolvedDataDir = [IO.Path]::GetFullPath($DataDir)
$configPath = Join-Path $resolvedDataDir 'config.toml'
if (-not (Test-Path -LiteralPath $resolvedDataDir -PathType Container)) {
  throw 'Muse 用户数据目录不存在。'
}
if (-not (Test-Path -LiteralPath $configPath -PathType Leaf)) {
  throw 'Muse 用户配置 config.toml 不存在。'
}

$currentIdentity = [Security.Principal.WindowsIdentity]::GetCurrent()
$currentUserSid = $currentIdentity.User.Value
$allowedSids = [Collections.Generic.HashSet[string]]::new(
  [StringComparer]::OrdinalIgnoreCase
)
foreach ($sid in @(
  $currentUserSid,
  'S-1-5-18',      # LOCAL SYSTEM
  'S-1-5-32-544',  # BUILTIN\Administrators
  'S-1-3-0'        # CREATOR OWNER
)) {
  [void]$allowedSids.Add($sid)
}

$exposureMask = [int64](
  [Security.AccessControl.FileSystemRights]::ReadData -bor
  [Security.AccessControl.FileSystemRights]::ReadAttributes -bor
  [Security.AccessControl.FileSystemRights]::ReadExtendedAttributes -bor
  [Security.AccessControl.FileSystemRights]::ReadPermissions -bor
  [Security.AccessControl.FileSystemRights]::WriteData -bor
  [Security.AccessControl.FileSystemRights]::AppendData -bor
  [Security.AccessControl.FileSystemRights]::WriteAttributes -bor
  [Security.AccessControl.FileSystemRights]::WriteExtendedAttributes -bor
  [Security.AccessControl.FileSystemRights]::Delete -bor
  [Security.AccessControl.FileSystemRights]::ChangePermissions -bor
  [Security.AccessControl.FileSystemRights]::TakeOwnership
)

function Assert-PrivateAcl {
  param(
    [Parameter(Mandatory = $true)]
    [string]$LiteralPath,

    [Parameter(Mandatory = $true)]
    [string]$Label
  )

  $acl = Get-Acl -LiteralPath $LiteralPath
  $ownerSid = $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
  # 管理员组成员创建的对象，所有者默认是 Administrators 组而非创建者本人（CI runner
  # 即如此）；这不放宽 DACL，普通本机用户仍无访问权，故所有者允许这两种主体。
  if ($ownerSid -ne $currentUserSid -and $ownerSid -ne 'S-1-5-32-544') {
    throw "$Label 的所有者不是当前用户或 Administrators 组。"
  }

  $currentUserCanAccess = $false
  $rules = $acl.GetAccessRules(
    $true,
    $true,
    [Security.Principal.SecurityIdentifier]
  )
  foreach ($rule in $rules) {
    if ($rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow) {
      continue
    }
    $sid = $rule.IdentityReference.Value
    $exposesContent = ([int64]$rule.FileSystemRights -band $exposureMask) -ne 0
    if (-not $exposesContent) {
      continue
    }
    if (-not $allowedSids.Contains($sid)) {
      throw "$Label 向非允许主体开放了文件内容或修改权限。"
    }
    if ($sid -eq $currentUserSid) {
      $currentUserCanAccess = $true
    }
  }

  if (-not $currentUserCanAccess) {
    throw "$Label 没有向当前用户授予文件访问权限。"
  }
}

Assert-PrivateAcl -LiteralPath $resolvedDataDir -Label 'Muse 用户数据目录'
Assert-PrivateAcl -LiteralPath $configPath -Label 'Muse 用户配置'

$probePath = Join-Path $resolvedDataDir ('.muse-acl-probe-' + [guid]::NewGuid() + '.tmp')
try {
  [IO.File]::WriteAllText($probePath, 'acl-probe')
  Assert-PrivateAcl -LiteralPath $probePath -Label 'Muse 原子写入临时文件'
}
finally {
  if (Test-Path -LiteralPath $probePath) {
    Remove-Item -LiteralPath $probePath -Force
  }
}

[pscustomobject]@{
  schema_version = 'muse-windows-acl-evidence/v1'
  data_directory = 'private'
  config_file = 'private'
  atomic_temporary_file = 'private'
} | ConvertTo-Json -Compress
