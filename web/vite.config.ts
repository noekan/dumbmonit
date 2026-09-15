import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

const PROXY = { target: 'http://localhost:8080', changeOrigin: true };

export default defineConfig({
	plugins: [tailwindcss(), sveltekit()],
	// En développement, le front tourne sur Vite et le backend Rust sur 8080.
	// Ce proxy évite toute configuration CORS côté serveur.
	// `preview` en bénéficie aussi, pour éprouver le build face au vrai backend.
	server: {
		proxy: { '/api': PROXY },
		// La documentation Markdown (`docs/` à la racine du dépôt) vit hors de `web/` :
		// Vite doit être autorisé à la servir en développement. Le build, lui,
		// l'embarque sans restriction. Les chemins sont relatifs à `web/`.
		fs: { allow: ['.', '../docs'] }
	},
	preview: { proxy: { '/api': PROXY } }
});
