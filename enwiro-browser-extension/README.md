# enwiro browser extension

Chromium (MV3) extension that recognizes web pages belonging to an enwiro
recipe - e.g. a GitHub PR or issue on a configured repo - and activates the
matching environment with one click on the toolbar action.

How it works: cookbooks attach URL rules to their pattern recipes
(`enwiro_sdk::url_rule`), the daemon validates them into `recipes.cache`,
and this extension pulls them over native messaging from `enw browser host`
and routes tab URLs against them client-side. All validation and activation
logic stays host-side; the extension is a dumb matcher.

## Setup

1. Have `enwiro-daemon` running - it installs the native messaging host
   manifest automatically (opt out with `browser_integration = false` in
   the enwiro config). Without the daemon, run `enw browser install` once.
2. Build and load the extension:

   ```sh
   pnpm install
   pnpm build
   ```

   Then open `chrome://extensions`, enable Developer mode, and "Load
   unpacked" pointing at `enwiro-browser-extension/dist/`. The `key` pinned
   in `manifest.json` keeps the extension ID stable, so the native
   messaging manifest's allowlist matches regardless of install path.

## Development

- `pnpm test` - vitest over the pure router logic (Node's native
  `URLPattern` matches Chrome's).
- `pnpm build` - typecheck + bundle the service worker into `dist/`.
