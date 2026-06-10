import { defineConfig } from '@hey-api/openapi-ts'

// Generates the typed client + TanStack Query options into src/client from the
// OpenAPI spec emitted by `enw-gui --dump-openapi`. Regenerate with `pnpm gen`.
export default defineConfig({
  input: './openapi.json',
  output: './src/client',
  plugins: ['@hey-api/client-fetch', '@tanstack/react-query'],
})
