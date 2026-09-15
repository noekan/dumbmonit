import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
	preprocess: vitePreprocess(),
	kit: {
		// Le binaire Rust embarque un site statique : pas de serveur Node en production.
		// `fallback` active le mode SPA, indispensable pour les routes dynamiques
		// comme /targets/42 qui sont résolues côté client.
		adapter: adapter({
			pages: 'build',
			assets: 'build',
			fallback: 'index.html',
			precompress: false,
			strict: false
		}),
		// Aucune page n'est pré-rendue par défaut : tout dépend de l'API au runtime.
		prerender: { entries: [] }
	}
};

export default config;
