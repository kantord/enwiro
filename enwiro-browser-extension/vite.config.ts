import { defineConfig } from 'vite'

// One entry, no HTML: the extension is a single MV3 service worker plus
// the static manifest and icon from public/.
export default defineConfig({
  build: {
    rollupOptions: {
      input: 'src/background.ts',
      output: {
        entryFileNames: 'background.js',
      },
    },
  },
})
