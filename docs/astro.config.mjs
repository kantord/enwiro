// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightThemeFlexoki from 'starlight-theme-flexoki';

// https://astro.build/config
export default defineConfig({
	integrations: [
		starlight({
			plugins: [starlightThemeFlexoki()],
			title: 'enwiro',
			social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/kantord/enwiro' }],
		}),
	],
});
