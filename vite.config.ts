import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

function toFileSystemPath(url: URL): string {
  const decodedPath = decodeURIComponent(url.pathname);
  return decodedPath.replace(/^\/([A-Za-z]:)/, '$1');
}

// 以直接依赖作为稳定根节点，Rollup 会把各自的传递依赖收进同一缓存组，
// 避免按单文件硬拆造成 vendor chunk 之间的循环引用。
const MANUAL_CHUNKS = {
  'vendor-react': ['react', 'react/jsx-runtime', 'react-dom', 'react-dom/client'],
  'vendor-markdown': ['react-markdown', 'remark-gfm'],
  'vendor-ui': [
    '@radix-ui/react-alert-dialog',
    '@radix-ui/react-toast',
    'lucide-react'
  ]
};

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      '@': toFileSystemPath(new URL('./src', import.meta.url))
    }
  },
  server: {
    host: '127.0.0.1',
    port: 5173,
    strictPort: true
  },
  preview: {
    host: '127.0.0.1',
    port: 5173,
    strictPort: true
  },
  build: {
    target: ['safari16.2', 'chrome111'],
    cssTarget: ['safari16.2', 'chrome111'],
    outDir: 'dist',
    emptyOutDir: true,
    assetsDir: 'ui-assets',
    rollupOptions: {
      output: {
        manualChunks: MANUAL_CHUNKS
      }
    }
  },
  test: {
    environment: 'jsdom',
    setupFiles: './src/test/setup.ts',
    include: ['src/**/*.test.{ts,tsx}']
  }
});
