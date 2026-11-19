import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';
import { defineConfig, loadEnv, type Plugin } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { nodePolyfills } from 'vite-plugin-node-polyfills';

const portalRoot = fileURLToPath(new URL('.', import.meta.url));
const GZIP_EXTENSIONS = new Set(['.html', '.htm', '.js', '.mjs', '.css', '.json', '.svg', '.txt', '.wasm']);

async function walkFiles(dir: string): Promise<string[]> {
  const entries = await fs.readdir(dir, { withFileTypes: true });
  const files: string[] = [];
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await walkFiles(full)));
    } else if (entry.isFile()) {
      files.push(full);
    }
  }
  return files;
}

/** Pre-compresses built static portal assets at zlib level 9 alongside raw files. */
function gzipStaticAssetsPlugin(): Plugin {
  let outDir = path.resolve(portalRoot, 'dist');
  return {
    name: 'pits-static-gzip',
    apply: 'build',
    configResolved(config) {
      outDir = path.resolve(config.root, config.build.outDir);
    },
    async closeBundle() {
      const files = await walkFiles(outDir);
      let count = 0;
      let rawBytes = 0;
      let gzBytes = 0;
      await Promise.all(
        files.map(async filePath => {
          const ext = path.extname(filePath).toLowerCase();
          if (filePath.endsWith('.gz') || !GZIP_EXTENSIONS.has(ext)) {
            return;
          }
          const raw = await fs.readFile(filePath);
          if (raw.length < 256) {
            return;
          }
          const gz = gzipSync(raw, { level: 9 });
          if (gz.length < raw.length) {
            await fs.writeFile(`${filePath}.gz`, gz);
            count += 1;
            rawBytes += raw.length;
            gzBytes += gz.length;
          }
        }),
      );
      console.log(
        `[pits-static-gzip] precompressed ${count} files: ${(rawBytes / 1024).toFixed(1)} KiB -> ${(gzBytes / 1024).toFixed(1)} KiB`,
      );
    },
  };
}

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, portalRoot, '');
  const apiProxyTarget =
    env.VITE_API_PROXY_TARGET ||
    process.env.VITE_API_PROXY_TARGET ||
    (mode === 'local' ? 'http://127.0.0.1:18090' : 'http://100.95.95.88:18090');

  return {
    root: portalRoot,
    base: '/',
    resolve: {
      dedupe: [
        'react',
        'react-dom',
        'react-router',
        'react-router-dom',
        '@emotion/react',
        '@emotion/styled',
      ],
    },
    plugins: [
      react(),
      tailwindcss(),
      nodePolyfills({
        include: ['buffer', 'process', 'util', 'stream', 'events', 'path', 'url'],
        globals: {
          Buffer: true,
          global: true,
          process: true,
        },
      }),
      gzipStaticAssetsPlugin(),
    ],
    define: {
      'process.env.NODE_ENV': JSON.stringify('production'),
    },
    build: {
      outDir: 'dist',
      emptyOutDir: true,
      sourcemap: false,
      target: 'es2022',
      chunkSizeWarningLimit: 3500,
      modulePreload: {
        resolveDependencies(_filename, deps) {
          return deps.filter(dep => !/highlight\.js|hljs/i.test(dep));
        },
      },
    },
    server: {
      port: 3000,
      proxy: {
        '/api': {
          target: apiProxyTarget,
          changeOrigin: true,
        },
      },
    },
  };
});
