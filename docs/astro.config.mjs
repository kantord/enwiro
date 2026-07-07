// @ts-check
import starlight from '@astrojs/starlight'
import { defineConfig } from 'astro/config'
import mermaid from 'astro-mermaid'
import starlightThemeFlexoki from 'starlight-theme-flexoki'

// https://astro.build/config
export default defineConfig({
  site: 'https://enwi.ro',
  integrations: [
    // Must come before starlight so it can transform ```mermaid code blocks
    // before Starlight's syntax highlighting. Renders client-side.
    mermaid({ theme: 'default', autoTheme: true }),
    starlight({
      plugins: [starlightThemeFlexoki()],
      title: 'enwiro',
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/kantord/enwiro',
        },
      ],
      sidebar: [
        { slug: 'index' },
        { slug: 'activating-workspaces' },
        { slug: 'launching-apps' },
        { slug: 'development-setup' },
        {
          label: 'Adapters',
          items: [
            { slug: 'adapters' },
            {
              label: 'Available Adapters',
              items: [
                { slug: 'adapters/available-adapters/i3wm' },
                { slug: 'adapters/available-adapters/tmux' },
              ],
            },
            { slug: 'adapters/creating-an-adapter' },
          ],
        },
        {
          label: 'Bridges',
          items: [{ slug: 'bridges' }, { slug: 'bridges/creating-a-bridge' }],
        },
      ],
    }),
  ],
})
