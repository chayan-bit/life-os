// Minimal test config, separate from vite.config.js so the dev/build config
// stays untouched. jsdom environment for React component tests (issue #121).
import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  resolve: {
    dedupe: ['react', 'react-dom'],
  },
  test: {
    environment: 'jsdom',
  },
})
