import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// `npm run dev` proxies the API to a local relay, e.g.
//   cargo run -- serve --listen 127.0.0.1:8443 --base-domain localhost --dev-self-signed
// The relay serves the dashboard only for its dashboard host, hence the Host rewrite.
const relay = process.env.VORP_RELAY ?? 'https://127.0.0.1:8443';
const proxy = { target: relay, secure: false, headers: { host: 'localhost' } };

export default defineConfig({
  plugins: [svelte()],
  server: { proxy: { '/api': proxy, '/healthz': proxy } },
  build: { target: 'es2022' },
});
