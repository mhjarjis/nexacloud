<script setup lang="ts">
// Put the panel on its own domain (nginx proxy + Let's Encrypt), so GitHub
// webhooks and browsers get a real certificate.
const domain = ref('')
const ok = computed(() => /^([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z0-9-]{2,63}$/.test(domain.value.trim().toLowerCase()))
const port = computed(() => (import.meta.client ? window.location.port || '443' : '8443'))
const created = ref(false)
</script>

<template>
  <section class="card card-pad stack" style="gap: 12px" aria-labelledby="pd-h">
    <h2 id="pd-h">Panel address</h2>
    <p class="small text-2">Give the panel its own domain (e.g. <span class="mono">panel.example.com</span>) with a free certificate — no more browser warning, and GitHub webhooks can deploy instantly. Point the domain's A record to this server first. Port {{ port }} keeps working as a fallback.</p>
    <label class="row wrap" style="gap: 8px">
      <span class="small">Domain</span>
      <input v-model="domain" class="in mono" type="text" placeholder="panel.example.com" style="max-width: 280px">
    </label>
    <PlanAction
      :key="domain"
      :request="{ kind: 'site_create', params: { kind: 'proxy', domain: domain.trim().toLowerCase(), upstream: `https://127.0.0.1:${port}`, options: { client_max_body_size: '64M', security_headers: true, hsts: false, cache_static: false, block_hidden: false } } }"
      label="Preview"
      apply-label="Create panel site"
      :disabled="!ok"
      success-text="Done. Now open Sites → this domain → Enable HTTPS."
      @done="(s: string) => (created = s === 'applied')"
    />
    <NuxtLink v-if="created" :to="`/sites?q=${encodeURIComponent(domain.trim().toLowerCase())}`" class="btn btn-s btn-p" style="align-self: flex-start">Enable HTTPS for {{ domain }}</NuxtLink>
  </section>
</template>

<style scoped>
.in { min-height: 40px; padding: 0 10px; background: var(--sunken, #0F1318); border: 1px solid var(--btn-line, #2A3441); border-radius: 8px; color: var(--text); font-size: 14px; }
</style>
