// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightThemeFlexoki from 'starlight-theme-flexoki';

// https://astro.build/config
export default defineConfig({
	site: 'https://enwi.ro',
	integrations: [
		starlight({
			plugins: [starlightThemeFlexoki()],
			title: 'enwiro',
			social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/kantord/enwiro' }],
			sidebar: [
				{ slug: 'index' },
				{ slug: 'activating-workspaces' },
				{ slug: 'launching-apps' },
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
			],
		}),
	],
});
