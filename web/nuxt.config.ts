// NexaCloud web UI — a pure client-side SPA. The Rust agent serves the
// generated files (`.output/public`) and the JSON API under `/api`.
export default defineNuxtConfig({
  compatibilityDate: '2026-11-01',
  ssr: false,
  devtools: { enabled: false },
  telemetry: false,
  css: ['~/assets/css/main.css'],
  app: {
    head: {
      htmlAttrs: { lang: 'en' },
      title: 'NexaCloud',
      meta: [
        { name: 'viewport', content: 'width=device-width, initial-scale=1' },
        { name: 'color-scheme', content: 'dark' },
        { name: 'theme-color', content: '#0B0E12' },
        { name: 'robots', content: 'noindex, nofollow' },
      ],
      link: [
        { rel: 'icon', type: 'image/svg+xml', href: '/favicon.svg' },
        { rel: 'preconnect', href: 'https://fonts.googleapis.com' },
        { rel: 'preconnect', href: 'https://fonts.gstatic.com', crossorigin: '' },
        {
          rel: 'stylesheet',
          href: 'https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&family=JetBrains+Mono:wght@400;500&family=Space+Grotesk:wght@500;600;700&display=swap',
        },
      ],
    },
  },
  nitro: {
    // `npm run dev` talks to a local agent started with `tls = false`.
    // changeOrigin stays off so the agent sees the browser's Host header,
    // which must match Origin for POST requests (CSRF check).
    devProxy: {
      '/api': { target: 'http://127.0.0.1:8443/api', changeOrigin: false },
    },
  },
  typescript: {
    strict: true,
  },
})
