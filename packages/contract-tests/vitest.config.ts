import { defineConfig } from 'vitest/config'

export default defineConfig({
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
    // Contract tests hit a live daemon — keep them sequential and generous
    fileParallelism: false,
    testTimeout: 15_000,
    hookTimeout: 15_000,
  },
})
