import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import path from 'path'
import fs from 'fs'

let backendPort = 3000
try {
  const configToml = fs.readFileSync(path.resolve(__dirname, '../config.toml'), 'utf-8')
  const portMatch = configToml.match(/^port\s*=\s*(\d+)/m)
  if (portMatch) backendPort = parseInt(portMatch[1], 10)
} catch {
  // config.toml may not exist during Docker builds (excluded by .dockerignore).
  // The proxy config is only used in dev mode; production builds ignore it.
}

export default defineConfig({
  base: '/admin/',
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    proxy: {
      '/admin/api': `http://127.0.0.1:${backendPort}`,
      '/v1': `http://127.0.0.1:${backendPort}`,
      '/anthropic': `http://127.0.0.1:${backendPort}`,
    },
  },
})
