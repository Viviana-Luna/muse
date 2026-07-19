import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), 'utf8');
}

function count(source, needle) {
  return source.split(needle).length - 1;
}

function manifestVersion(relativePath) {
  const manifest = read(relativePath);
  const match = manifest.match(/^version = "([^"]+)"$/m);
  assert.ok(match, `${relativePath} 缺少包版本字段。`);
  return match[1];
}

test('CI 触发器只覆盖约定的长期分支、正式标签和手动测试包入口', () => {
  const workflow = read('.github/workflows/ci.yml');
  const triggerBlock = workflow.slice(0, workflow.indexOf('\npermissions:'));

  assert.match(
    triggerBlock,
    /on:\n  push:\n    branches:\n      - dev\n      - master\n    tags:\n      - "v\*"\n  pull_request:\n    branches:\n      - dev\n      - master\n  workflow_dispatch:/,
  );
  assert.equal(count(workflow, 'contents: write'), 1, '只有 Release job 可以取得写权限。');
  assert.match(workflow, /permissions:\n  contents: read/);
  assert.doesNotMatch(workflow, /\bsecrets\./, 'CI 不得读取真实供应商密钥。');

  assert.match(
    workflow,
    /package_policy:\n[\s\S]*github\.event_name == 'workflow_dispatch' && inputs\.build_test_packages/,
    '手动测试包请求必须进入独立策略校验 job。',
  );
  assert.match(
    workflow,
    /test "\$\{GITHUB_REF\}" = "refs\/heads\/dev"/,
    '非 dev 的手动测试包请求必须明确失败。',
  );
  assert.match(
    workflow,
    /git merge-base --is-ancestor "\$\{tag_commit\}" "refs\/remotes\/origin\/master"/,
    '正式标签必须校验提交属于 master。',
  );
  assert.match(
    workflow,
    /github\.event_name == 'workflow_dispatch' &&\n      inputs\.build_test_packages && github\.ref == 'refs\/heads\/dev'/,
    '打包 job 只允许 dev 手动测试包或正式标签。',
  );
  assert.match(
    workflow,
    /release:\n[\s\S]*if: startsWith\(github\.ref, 'refs\/tags\/v'\)[\s\S]*permissions:\n      contents: write/,
  );
});

test('CI 全面使用根前端、src-tauri 和 crates 工作区', () => {
  const workflow = read('.github/workflows/ci.yml');

  for (const legacy of ['muse-ui', 'muse-desktop', 'muse-agent', 'TAURI_FRONTEND_PATH']) {
    assert.ok(!workflow.includes(legacy), `CI 仍包含旧工程残留：${legacy}`);
  }
  assert.equal(count(workflow, 'cache-dependency-path: package-lock.json'), 2);
  assert.equal(count(workflow, 'run: npm ci'), 2);
  assert.equal(count(workflow, 'run: npm run tauri -- build'), 2);
  assert.match(workflow, /cargo test -p muse --all-targets --locked/);
  assert.doesNotMatch(
    workflow,
    /cargo test -p muse-local-api security:: --all-targets --locked/,
    '本地 API 安全测试已包含在 workspace 全量测试中，不得重复执行。',
  );
  assert.match(workflow, /live_tests:[\s\S]*--features live-tests/);
  assert.equal(count(workflow, 'run: npm run typecheck'), 0);
  assert.equal(count(workflow, 'run: npm run check:bundle-size'), 0);
  assert.match(workflow, /bash scripts\/clean-build-artifacts\.test\.sh/);
  assert.doesNotMatch(workflow, /working-directory:/);
  assert.doesNotMatch(workflow, /cargo install tauri-cli/);
  assert.ok(
    !fs.existsSync(path.join(repoRoot, 'script')),
    '仓库脚本必须统一放在 scripts/，不得重新引入根目录 script/。',
  );

  const tracked = execFileSync('git', ['ls-files'], {
    cwd: repoRoot,
    encoding: 'utf8',
  });
  assert.doesNotMatch(tracked, /^(?:muse-ui|muse-desktop|muse-agent)\//m);
});

test('标准 Tauri 壳、内嵌页面、图标和版本保持一致', () => {
  const workspace = read('Cargo.toml');
  const workflow = read('.github/workflows/ci.yml');
  for (const member of [
    'crates/muse-core',
    'crates/muse-runtime',
    'crates/muse-local-api',
    'src-tauri',
  ]) {
    assert.ok(workspace.includes(`"${member}"`), `workspace 缺少成员 ${member}。`);
  }
  assert.match(workspace, /default-members = \["src-tauri"\]/);
  assert.doesNotMatch(workspace, /muse-(?:ui|desktop|agent)/);

  const main = read('src-tauri/src/main.rs').replace(/\s+/g, ' ').trim();
  assert.equal(
    main,
    '#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] fn main() { muse_lib::run(); }',
    'src-tauri/src/main.rs 必须保持为薄桌面入口。',
  );

  const desktop = read('src-tauri/src/lib.rs');
  assert.match(desktop, /WebviewUrl::App\("index\.html"\.into\(\)\)/);
  assert.doesNotMatch(desktop, /WebviewUrl::External/);
  const plugin = desktop.indexOf('.plugin(tauri_plugin_single_instance::init');
  const setup = desktop.indexOf('.setup(move |app|');
  assert.ok(plugin >= 0 && plugin < setup, '单实例插件必须先于桌面 setup 注册。');
  assert.match(desktop, /title_bar_style\(tauri::TitleBarStyle::Overlay\)/);
  assert.match(desktop, /hidden_title\(true\)/);
  assert.match(desktop, /window_builder\.decorations\(false\)/);

  const handlerFacade = read('crates/muse-local-api/src/handlers.rs');
  assert.ok(handlerFacade.split('\n').length <= 100, 'handler façade 只能承担装配与兼容导出。');
  for (const relative of [
    'crates/muse-local-api/src/handlers/implementation.rs',
    'crates/muse-local-api/src/handlers/runtime_api.rs',
    'crates/muse-local-api/src/handlers/api/runtime_assets_voice.rs',
    'crates/muse-local-api/src/handlers/api/app_preferences.rs',
    'crates/muse-local-api/src/handlers/api/chat_sessions_models.rs',
    'crates/muse-local-api/src/handlers/api/personas_stream.rs',
    'crates/muse-local-api/src/handlers/api/persona_session_binding.rs',
    'crates/muse-local-api/src/handlers/tools/registry.rs',
    'crates/muse-local-api/src/handlers/tools/interaction.rs',
    'crates/muse-local-api/src/handlers/tools/files.rs',
    'crates/muse-local-api/src/handlers/tools/command.rs',
    'crates/muse-local-api/src/handlers/tools/network.rs',
    'crates/muse-local-api/src/handlers/tools/mcp.rs',
    'crates/muse-local-api/src/handlers/tools/session.rs',
    'crates/muse-local-api/src/handlers/tools/persona_context.rs',
  ]) {
    assert.ok(read(relative).split('\n').length <= 3500, `${relative} 超过 3500 行。`);
  }
  const router = read('crates/muse-local-api/src/router.rs');
  assert.doesNotMatch(router.split('#[cfg(test)]')[0], /\/model-assets/);
  const windowsJob = read('crates/muse-core/src/process_supervision.rs');
  for (const marker of [
    'JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE',
    'AssignProcessToJobObject',
    'TerminateJobObject',
    'CREATE_NO_WINDOW',
  ]) {
    assert.ok(windowsJob.includes(marker), `Windows Job Object 契约缺少 ${marker}。`);
  }
  assert.ok(!windowsJob.includes('taskkill'), 'Windows 进程回收不得退回 taskkill。');

  const windowsAclCheck = read('scripts/check-windows-private-acl.ps1');
  for (const marker of [
    'muse-windows-acl-evidence/v1',
    'S-1-5-18',
    'S-1-5-32-544',
    'GetAccessRules',
    '.muse-acl-probe-',
  ]) {
    assert.ok(windowsAclCheck.includes(marker), `Windows ACL 验收缺少 ${marker}。`);
  }
  assert.doesNotMatch(
    windowsAclCheck,
    /api[_-]?key|credential|password|secret/iu,
    'Windows ACL 验收不得读取或输出秘密。',
  );
  assert.equal(
    count(workflow, '& ./scripts/check-windows-private-acl.ps1 -DataDir $expectedDataDir'),
    1,
    'Windows NSIS 冒烟必须执行一次用户数据 ACL 验收。',
  );

  const windowsAcceptanceEvidence = read('scripts/collect-windows-acceptance-evidence.ps1');
  for (const marker of [
    'muse-windows-acceptance-evidence/v1',
    'Win32_OperatingSystem',
    'ProductType',
    'windows_10_22h2',
    'windows_11',
    'F3017226-FE2A-4295-8BDF-00C3A9A7E4C5',
    'WebView2 Runtime',
    "[version]'111.0.0.0'",
    'LogPixels',
    'AppsUseLightTheme',
    'SystemUsesLightTheme',
    'GetDpiForWindow',
    'Get-FileHash',
    'Get-DescendantProcessCount',
    'ExpectNoMuseProcess',
  ]) {
    assert.ok(
      windowsAcceptanceEvidence.includes(marker),
      `Windows 实机证据采集缺少 ${marker}。`,
    );
  }
  assert.doesNotMatch(
    windowsAcceptanceEvidence,
    /UserName|ComputerName|CommandLine|Get-Content|config\.toml/iu,
    'Windows 实机证据不得输出身份、命令行或配置内容。',
  );

  const tauriConfig = JSON.parse(read('src-tauri/tauri.conf.json'));
  assert.equal(tauriConfig.build.frontendDist, '../dist');
  assert.equal(tauriConfig.build.beforeBuildCommand, 'npm run build');
  assert.equal(tauriConfig.build.beforeDevCommand, 'npm run dev');
  assert.deepEqual(tauriConfig.bundle.icon, [
    'icons/muse-logo.png',
    'icons/muse-logo.icns',
    'icons/muse-logo.ico',
  ]);
  assert.equal(tauriConfig.bundle.macOS.minimumSystemVersion, '13.1');
  assert.equal(tauriConfig.bundle.macOS.infoPlist, 'Info.plist');
  assert.equal(tauriConfig.bundle.macOS.entitlements, 'Entitlements.plist');
  const macInfoPlist = read('src-tauri/Info.plist');
  assert.match(macInfoPlist, /<key>NSMicrophoneUsageDescription<\/key>/);
  assert.match(
    macInfoPlist,
    /<string>[^<]*语音识别服务[^<]*<\/string>/,
    '麦克风用途说明必须解释录音会发送到用户配置的语音识别服务。'
  );
  const macEntitlements = read('src-tauri/Entitlements.plist');
  assert.match(macEntitlements, /<key>com\.apple\.security\.device\.audio-input<\/key>\s*<true\/>/);
  assert.equal(
    tauriConfig.bundle.windows.webviewInstallMode.type,
    'downloadBootstrapper',
  );
  assert.equal(tauriConfig.bundle.windows.nsis.minimumWebview2Version, '111.0.0.0');
  assert.match(
    tauriConfig.app.security.csp,
    /connect-src 'self' ipc: http:\/\/ipc\.localhost/,
    'WebView CSP 必须允许 Tauri 在 macOS 和 Windows 使用的精确 IPC 协议源。',
  );

  const png = fs.readFileSync(path.join(repoRoot, 'src-tauri/icons/muse-logo.png'));
  assert.equal(png.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
  const ico = fs.readFileSync(path.join(repoRoot, 'src-tauri/icons/muse-logo.ico'));
  assert.equal(ico.subarray(0, 4).toString('hex'), '00000100');
  const icns = fs.readFileSync(path.join(repoRoot, 'src-tauri/icons/muse-logo.icns'));
  assert.equal(icns.subarray(0, 4).toString('ascii'), 'icns');
  assert.equal(icns.readUInt32BE(4), icns.length);

  const packageJson = JSON.parse(read('package.json'));
  const packageLock = JSON.parse(read('package-lock.json'));
  const versions = [
    packageJson.version,
    packageLock.version,
    packageLock.packages[''].version,
    tauriConfig.version,
    manifestVersion('src-tauri/Cargo.toml'),
    manifestVersion('crates/muse-core/Cargo.toml'),
    manifestVersion('crates/muse-runtime/Cargo.toml'),
    manifestVersion('crates/muse-local-api/Cargo.toml'),
  ];
  assert.equal(new Set(versions).size, 1, `工程版本不一致：${versions.join(', ')}`);
});

test('WebView 构建目标、安装下限和公开支持口径保持一致', () => {
  const viteConfig = read('vite.config.ts');
  assert.match(viteConfig, /target: \['safari16\.2', 'chrome111'\]/);
  assert.match(viteConfig, /cssTarget: \['safari16\.2', 'chrome111'\]/);

  const styleRoot = path.join(repoRoot, 'src/assets/styles');
  const styles = fs
    .readdirSync(styleRoot, { withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith('.css'))
    .map((entry) => fs.readFileSync(path.join(styleRoot, entry.name), 'utf8'))
    .join('\n');
  assert.match(styles, /color-mix\(/, '前端能力基线应覆盖实际使用的 color-mix()。');
  assert.match(styles, /backdrop-filter/, '前端能力基线应覆盖实际使用的 backdrop-filter。');

  const builtStyles = fs
    .readdirSync(path.join(repoRoot, 'dist/ui-assets'), { withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith('.css'))
    .map((entry) => read(`dist/ui-assets/${entry.name}`))
    .join('\n');
  assert.match(builtStyles, /color-mix\(/, '生产 CSS 不得移除运行时主题混色。');
  assert.match(
    builtStyles,
    /-webkit-backdrop-filter/,
    'Safari 16.2 产物必须包含 backdrop-filter 前缀。',
  );
  assert.match(
    builtStyles,
    /-webkit-mask-image/,
    'Safari 16.2 产物必须包含立绘 mask-image 前缀。',
  );

  const publicSupportFiles = [
    'README.md',
    '.github/CONTRIBUTING.md',
    '.github/ISSUE_TEMPLATE/bug_report.md',
    'docs/releases/v0.1.0.md',
    'docs/webview-compatibility.md',
  ];
  const publicSupport = publicSupportFiles.map(read).join('\n');
  assert.doesNotMatch(publicSupport, /macOS 10\.15/i);
  for (const required of ['macOS 13.1', 'Windows 10 22H2', 'WebView2 111']) {
    assert.ok(publicSupport.includes(required), `公开支持口径缺少 ${required}。`);
  }
  const compatibilityGuide = read('docs/webview-compatibility.md');
  for (const marker of [
    'scripts/collect-windows-acceptance-evidence.ps1',
    '-MainProcessId',
    '-ExpectNoMuseProcess',
  ]) {
    assert.ok(compatibilityGuide.includes(marker), `Windows 实机指南缺少 ${marker}。`);
  }
});

test('生产构建产物可以由 Tauri 完整内嵌', () => {
  const distRoot = path.join(repoRoot, 'dist');
  const htmlPath = path.join(distRoot, 'index.html');
  assert.ok(fs.statSync(htmlPath).isFile(), '生产构建缺少 dist/index.html。');

  const html = fs.readFileSync(htmlPath, 'utf8');
  assert.doesNotMatch(html, /(?:src|href)=["']\/src\//);
  for (const [, asset] of html.matchAll(/(?:src|href)=["']([^"']+)["']/g)) {
    if (/^(?:[a-z]+:|#|data:|blob:)/i.test(asset)) continue;
    const relative = asset.replace(/^\.\//, '').replace(/^\//, '');
    assert.ok(fs.existsSync(path.join(distRoot, relative)), `内嵌资源不存在：${asset}`);
  }
});
