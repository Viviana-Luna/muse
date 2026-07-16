import { readdir, readFile, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const ENTRY_MAX_BYTES = 300 * 1024;
export const CHUNK_MAX_BYTES = 500 * 1024;

// 当前没有豁免项。未来确需豁免时必须填写 dist 下的精确相对路径，并在评审中说明原因。
export const CHUNK_SIZE_WHITELIST = new Set();

function normalizeAssetPath(asset) {
  const withoutQuery = asset.split(/[?#]/u, 1)[0];
  return withoutQuery.replace(/^\.\//u, '').replace(/^\//u, '');
}

function entryScriptsFromHtml(html) {
  const entries = new Set();
  for (const match of html.matchAll(/<script\b([^>]*)>/giu)) {
    const attributes = match[1];
    if (!/\btype=["']module["']/iu.test(attributes)) continue;
    const source = attributes.match(/\bsrc=["']([^"']+)["']/iu)?.[1];
    if (!source) continue;
    if (/^(?:[a-z]+:|\/\/)/iu.test(source)) {
      throw new Error(`入口脚本不得引用外部地址：${source}`);
    }
    const normalized = normalizeAssetPath(source);
    if (normalized.endsWith('.js')) entries.add(normalized);
  }
  if (entries.size === 0) {
    throw new Error('dist/index.html 没有可校验的 module 入口脚本。');
  }
  return entries;
}

async function collectJavaScriptFiles(root, current = root) {
  const files = [];
  for (const entry of await readdir(current, { withFileTypes: true })) {
    const absolute = path.join(current, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await collectJavaScriptFiles(root, absolute)));
    } else if (entry.isFile() && entry.name.endsWith('.js')) {
      files.push(path.relative(root, absolute).split(path.sep).join('/'));
    }
  }
  return files.sort();
}

export async function validateBundleSizes(
  distDir,
  {
    entryMaxBytes = ENTRY_MAX_BYTES,
    chunkMaxBytes = CHUNK_MAX_BYTES,
    whitelist = CHUNK_SIZE_WHITELIST
  } = {}
) {
  const indexPath = path.join(distDir, 'index.html');
  const entries = entryScriptsFromHtml(await readFile(indexPath, 'utf8'));
  const javaScriptFiles = await collectJavaScriptFiles(distDir);
  const failures = [];
  const measurements = [];

  for (const relativePath of entries) {
    if (!javaScriptFiles.includes(relativePath)) {
      failures.push(`入口脚本不存在：${relativePath}`);
    }
  }
  for (const relativePath of whitelist) {
    if (!javaScriptFiles.includes(relativePath)) {
      failures.push(`chunk 白名单包含不存在的脚本：${relativePath}`);
    }
  }

  for (const relativePath of javaScriptFiles) {
    const bytes = (await stat(path.join(distDir, relativePath))).size;
    const isEntry = entries.has(relativePath);
    const isWhitelisted = whitelist.has(relativePath);
    measurements.push({ relativePath, bytes, isEntry, isWhitelisted });

    if (isEntry && bytes > entryMaxBytes) {
      failures.push(
        `入口脚本 ${relativePath} 为 ${bytes} 字节，超过 ${entryMaxBytes} 字节上限。`
      );
    }
    if (!isEntry && !isWhitelisted && bytes > chunkMaxBytes) {
      failures.push(
        `脚本 chunk ${relativePath} 为 ${bytes} 字节，超过 ${chunkMaxBytes} 字节上限。`
      );
    }
  }

  if (failures.length > 0) {
    throw new Error(`前端产物体积门禁失败：\n- ${failures.join('\n- ')}`);
  }
  return measurements;
}

async function main() {
  const scriptDir = path.dirname(fileURLToPath(import.meta.url));
  const distDir = path.resolve(scriptDir, '..', 'dist');
  const measurements = await validateBundleSizes(distDir);
  for (const measurement of measurements) {
    const role = measurement.isEntry
      ? '入口'
      : measurement.isWhitelisted
        ? '白名单 chunk'
        : 'chunk';
    console.log(`${role} ${measurement.relativePath}：${measurement.bytes} 字节`);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
