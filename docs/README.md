# enwiro documentation

Source for the enwiro documentation site, published at
[enwi.ro](https://enwi.ro). Built with [Astro](https://astro.build) and
[Starlight](https://starlight.astro.build).

## Layout

- `src/content/docs/` - the published pages; the sidebar is configured in
  `astro.config.mjs`
- `adr/` - architecture decision records (not published on the site)
- `creating-a-cookbook.md`, `i3-workspace-rebalancing.md` - contributor
  specs, currently not published on the site

## Development

```sh
pnpm install
pnpm dev      # local dev server
pnpm build    # production build
```
