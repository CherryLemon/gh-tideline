import { defineConfig, type PluginOption } from 'vite'
import react from '@vitejs/plugin-react'

const BACKEND = 'http://127.0.0.1:7457'

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
      // changeOrigin rewrites Host to 127.0.0.1:7457, which the backend's Host check accepts.
      proxy: useMock
        ? undefined
        : {
            '/api': { target: BACKEND, changeOrigin: true },
            '/__auth': { target: BACKEND, changeOrigin: true },
          },
    },
    build: {
      outDir: 'dist',
      emptyOutDir: true,
      chunkSizeWarningLimit: 4096,
    },
  }
})
