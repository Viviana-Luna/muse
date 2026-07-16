import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { validateBundleSizes } from './check-bundle-size.mjs';

async function createFixture({ entryBytes, chunkBytes }) {
  const root = await mkdtemp(path.join(os.tmpdir(), 'muse-bundle-size-'));
  const assets = path.join(root, 'ui-assets');
  await mkdir(assets);
  await writeFile(
    path.join(root, 'index.html'),
    '<!doctype html><script type="module" src="/ui-assets/index.js"></script>'
  );
  await writeFile(path.join(assets, 'index.js'), Buffer.alloc(entryBytes, 1));
  await writeFile(path.join(assets, 'lazy.js'), Buffer.alloc(chunkBytes, 2));
  return root;
}

test('低于入口与 chunk 上限的产物通过', async (context) => {
  const root = await createFixture({ entryBytes: 10, chunkBytes: 20 });
  context.after(() => rm(root, { recursive: true, force: true }));
  const measurements = await validateBundleSizes(root, {
    entryMaxBytes: 10,
    chunkMaxBytes: 20
  });
  assert.equal(measurements.length, 2);
});

test('入口脚本超过上限时失败', async (context) => {
  const root = await createFixture({ entryBytes: 11, chunkBytes: 20 });
  context.after(() => rm(root, { recursive: true, force: true }));
  await assert.rejects(
    validateBundleSizes(root, { entryMaxBytes: 10, chunkMaxBytes: 20 }),
    /入口脚本.*超过/u
  );
});

test('非白名单 chunk 超过上限时失败', async (context) => {
  const root = await createFixture({ entryBytes: 10, chunkBytes: 21 });
  context.after(() => rm(root, { recursive: true, force: true }));
  await assert.rejects(
    validateBundleSizes(root, { entryMaxBytes: 10, chunkMaxBytes: 20 }),
    /脚本 chunk.*超过/u
  );
});

test('只有精确列入白名单的 chunk 可以超过通用上限', async (context) => {
  const root = await createFixture({ entryBytes: 10, chunkBytes: 21 });
  context.after(() => rm(root, { recursive: true, force: true }));
  await validateBundleSizes(root, {
    entryMaxBytes: 10,
    chunkMaxBytes: 20,
    whitelist: new Set(['ui-assets/lazy.js'])
  });
});

test('白名单中的脚本不存在时失败，避免陈旧豁免静默残留', async (context) => {
  const root = await createFixture({ entryBytes: 10, chunkBytes: 20 });
  context.after(() => rm(root, { recursive: true, force: true }));
  await assert.rejects(
    validateBundleSizes(root, {
      entryMaxBytes: 10,
      chunkMaxBytes: 20,
      whitelist: new Set(['ui-assets/missing.js'])
    }),
    /白名单包含不存在的脚本/u
  );
});
