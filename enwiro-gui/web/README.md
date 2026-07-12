# enw-gui frontend

React + TypeScript frontend for `enw-gui`, the enwiro web GUI. It renders a
kanban board of environments (Ready / Active / Waiting / Done) with
drag-and-drop and vim-style keyboard controls.

The production build (`dist/`) is embedded into the `enw-gui` binary via
`rust-embed`, so end users only need `cargo install enwiro-gui`.

## Development

```sh
pnpm install
pnpm dev      # Vite dev server
pnpm build    # tsc + vite build (output consumed by the Rust crate)
pnpm gen      # regenerate the API client from the backend's OpenAPI spec
```

`pnpm gen` runs `openapi-ts` against the spec dumped by `enw-gui
--dump-openapi`, so the Rust backend must be built first.

The GUI talks to the backend's `/api` routes, authenticated with the
capability token that `enw-gui` prints as part of its startup URL. Board data
comes from the enwiro daemon.
