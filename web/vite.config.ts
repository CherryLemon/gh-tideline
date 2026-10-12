import { defineConfig, type PluginOption, type ProxyOptions } from 'vite'
import react from '@vitejs/plugin-react'

const BACKEND = 'http://127.0.0.1:7457'

const DEV_ORIGINS = new Set(['http://localhost:5173', 'http://127.0.0.1:5173'])

// changeOrigin rewrites Host to 127.0.0.1:7457, which the backend's Host check accepts.
// Non-GET requests also need a backend Origin; only this dev server's own pages get one,
// so other origins still fail the backend's check.
const proxy: Record<string, ProxyOptions> = {
  '/api': {
    target: BACKEND,
    changeOrigin: true,
    configure: (server) =>
      server.on('proxyReq', (req) => {
        const origin = req.getHeader('origin')
        if (typeof origin === 'string' && DEV_ORIGINS.has(origin)) req.setHeader('origin', BACKEND)
      }),
  },
  '/__auth': { target: BACKEND, changeOrigin: true },
}

// `pnpm dev:mock` (vite --mode mock) serves a fake PR from mock/ instead of
// proxying to the Rust backend. The mock is only loaded by the dev server and
// never reaches the production bundle.
export default defineConfig(async ({ command, mode }) => {
  const useMock = command === 'serve' && mode === 'mock'
  const plugins: PluginOption[] = [react()]
  if (useMock) {
    const { mockApi } = await import('./mock/plugin.ts')
    plugins.push(mockApi())
  }
  return {
    plugins,
    server: {
      port: 5173,
      strictPort: true,
      proxy: useMock ? undefined : proxy,
    },
    build: {
      outDir: 'dist',
      emptyOutDir: true,
      chunkSizeWarningLimit: 4096,
    },
  }
})
