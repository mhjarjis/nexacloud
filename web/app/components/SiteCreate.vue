<script setup lang="ts">
// "New site" wizard → plan `site_create` (preview → apply).
import type { PhpInfo } from '~/types/api'

const props = withDefaults(defineProps<{ initialDomain?: string; title?: string }>(), { initialDomain: '', title: 'New site' })
const emit = defineEmits<{ done: []; close: [] }>()
const php = useResource<PhpInfo>('/api/php')

const KINDS = [
  { id: 'laravel', label: 'Laravel', hint: 'public/ web root, artisan' },
  { id: 'php', label: 'Plain PHP', hint: 'OOP / custom PHP app' },
  { id: 'static', label: 'Static', hint: 'HTML or built SPA' },
  { id: 'proxy', label: 'Proxy', hint: 'Node, Reverb, any port' },
  { id: 'redirect', label: 'Redirect', hint: '301 to another URL' },
] as const

const kind = ref<string>('laravel')
const domain = ref(props.initialDomain)
watch(() => props.initialDomain, (v) => { domain.value = v })
const aliases = ref('')
const root = ref('')
const phpVersion = ref('')
const upstream = ref('http://127.0.0.1:3000')
const redirectTo = ref('https://')
const redirectCode = ref(301)
const bodySize = ref('64M')

const usesPhp = computed(() => kind.value === 'laravel' || kind.value === 'php')
const installed = computed(() => (php.data.value?.versions ?? []).filter((v) => v.fpm_installed).map((v) => v.version))
watch(installed, (v) => {
  if (!phpVersion.value && v.length) phpVersion.value = [...v].sort().reverse()[0]!
}, { immediate: true })

const d = computed(() => domain.value.trim().toLowerCase())
const defaultRoot = computed(() => (d.value ? `/var/www/${d.value}/public` : '/var/www/<domain>/public'))
const domainOk = computed(() => /^([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z0-9-]{2,63}$/.test(d.value))

const request = computed(() => ({
  kind: 'site_create',
  params: {
    kind: kind.value,
    domain: d.value,
    aliases: aliases.value.split(/[\s,]+/).map((a) => a.trim().toLowerCase()).filter(Boolean),
    root: ['laravel', 'php', 'static'].includes(kind.value) && root.value.trim() ? root.value.trim() : null,
    php_version: usesPhp.value ? phpVersion.value : null,
    upstream: kind.value === 'proxy' ? upstream.value.trim() : null,
    redirect_to: kind.value === 'redirect' ? redirectTo.value.trim() : null,
    redirect_code: kind.value === 'redirect' ? redirectCode.value : null,
    options: { client_max_body_size: bodySize.value, security_headers: true, hsts: false, cache_static: true, block_hidden: true },
  },
}))
const planKey = computed(() => JSON.stringify(request.value))
</script>

<template>
  <section class="card stack" style="padding: 20px 22px; gap: 18px" aria-labelledby="newsite-h">
    <div class="row" style="justify-content: space-between; gap: 12px">
      <div class="stack" style="gap: 4px">
        <h2 id="newsite-h">{{ title }}</h2>
        <p class="small muted">Creates the nginx site, a web root with a placeholder page and checks it answers. Add HTTPS right after — the domain must point to this server.</p>
      </div>
      <button type="button" class="btn btn-s btn-ghost" @click="emit('close')">Close</button>
    </div>

    <fieldset class="stack" style="gap: 8px; border: 0; padding: 0; margin: 0">
      <legend class="lbl" style="margin-bottom: 8px">What is it?</legend>
      <div class="kind-grid">
        <button
          v-for="k in KINDS"
          :key="k.id"
          type="button"
          class="kind"
          :class="{ on: kind === k.id }"
          :aria-pressed="kind === k.id"
          @click="kind = k.id"
        >
          <strong>{{ k.label }}</strong><span class="small muted">{{ k.hint }}</span>
        </button>
      </div>
    </fieldset>

    <div class="form-grid">
      <label class="field">
        <span>Domain or subdomain</span>
        <input v-model="domain" class="in" type="text" placeholder="blog.example.com" autocomplete="off" spellcheck="false">
        <span v-if="domain && !domainOk" class="small" style="color: var(--warn)">Use a full domain like blog.example.com</span>
      </label>
      <label class="field">
        <span>Also answer to (optional)</span>
        <input v-model="aliases" class="in" type="text" placeholder="www.blog.example.com" spellcheck="false">
      </label>
      <label v-if="usesPhp" class="field">
        <span>PHP version</span>
        <SearchSelect v-model="phpVersion" :options="installed.map((v) => ({ value: v, label: `PHP ${v}` }))" placeholder="Choose PHP version…" aria-label="PHP version" />
        <span v-if="!installed.length" class="small" style="color: var(--warn)">No PHP-FPM installed — install one on the PHP page.</span>
      </label>
      <label v-if="['laravel', 'php', 'static'].includes(kind)" class="field">
        <span>Web root</span>
        <input v-model="root" class="in mono" type="text" :placeholder="defaultRoot" spellcheck="false">
      </label>
      <label v-if="kind === 'proxy'" class="field">
        <span>Forward to</span>
        <input v-model="upstream" class="in mono" type="text" placeholder="http://127.0.0.1:3000" spellcheck="false">
      </label>
      <template v-if="kind === 'redirect'">
        <label class="field">
          <span>Redirect to</span>
          <input v-model="redirectTo" class="in mono" type="text" placeholder="https://www.example.com" spellcheck="false">
        </label>
        <label class="field">
          <span>Type</span>
          <SearchSelect v-model="redirectCode" :options="[{ value: 301, label: '301 permanent' }, { value: 302, label: '302 temporary' }]" aria-label="Redirect type" />
        </label>
      </template>
      <label class="field">
        <span>Max upload size</span>
        <SearchSelect v-model="bodySize" :options="['16M', '64M', '128M', '512M', '1G']" aria-label="Max upload size" />
      </label>
    </div>

    <PlanAction
      :key="planKey"
      :request="request"
      label="Preview"
      icon="plus"
      btn-class="btn btn-p"
      apply-label="Create site"
      success-text="Site created. Point the domain's A record to this server, then enable HTTPS from the site row."
      :disabled="!domainOk || (usesPhp && !phpVersion)"
      @done="(s) => s === 'applied' && emit('done')"
    />
  </section>
</template>

<style scoped>
.kind-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 8px; }
.kind { display: flex; flex-direction: column; gap: 4px; align-items: flex-start; text-align: left; padding: 12px; min-height: 64px;
  border-radius: 10px; border: 1px solid var(--btn-line, #2A3441); background: var(--sunken, #0F1318); color: var(--text); cursor: pointer; }
.kind.on { border-color: var(--accent, #3DDC97); background: #13211B; }
.form-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); gap: 14px; }
.field { display: flex; flex-direction: column; gap: 6px; }
.field > span:first-child { font-size: 13px; font-weight: 500; color: var(--text-2); }
.in { min-height: 44px; padding: 0 12px; background: var(--sunken, #0F1318); border: 1px solid var(--btn-line, #2A3441); border-radius: 8px; color: var(--text); font-size: 14px; width: 100%; box-sizing: border-box; }
</style>
