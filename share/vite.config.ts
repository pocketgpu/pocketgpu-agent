import { svelte } from '@sveltejs/vite-plugin-svelte';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

// A static page (F48): built files only, hashed names so browsers keep them a year.
export default defineConfig({
	plugins: [tailwindcss(), svelte()],
	build: { target: 'es2022', assetsInlineLimit: 0 },
	test: { environment: 'node', include: ['src/**/*.spec.ts'] }
});
