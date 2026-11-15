<script setup lang="ts">
// Per-site actions shown in the expanded Sites row.
import type { PhpInfo, Site, SiteModel } from '~/types/api'

const props = defineProps<{ site: Site; php: PhpInfo | null; initialTab?: string }>()
const emit = defineEmits<{ changed: [] }>()
const api = useApi()

const managed = computed(() => props.site.config_path.startsWith('/etc/nginx/conf.d/nexacloud-'))
const model = ref<SiteModel | null>(null)
const tab = ref<'' | 'php' | 'ssl' | 'settings' | 'nginx' | 'remove' | 'runtime' | 'adopt' | 'isolate'>((props.initialTab as '') ?? '')
const acceptCustom = ref(false)

async function loadModel() {
  if (!managed.value) return
  try {
    model.value = await api.get<SiteModel>(`/api/sites/${encodeURIComponent(props.site.id)}/model`)
    opts.value = { ...model.value.options }
    aliasText.value = model.value.domains.slice(1).join(' ')
  } catch { model.value = null }
}
onMounted(loadModel)

// Runtime choice
type Rt = 'fpm' | 'frankenphp' | 'octane'
const runtimeTarget = ref<Rt>('fpm')
watch(model, (m) => { if (m) runtimeTarget.value = m.runtime === 'fpm' ? (m.kind === 'laravel' ? 'octane' : 'frankenphp') : 'fpm' }, { immediate: true })
const runtimes = computed(() => {
  const m = model.value
  const laravel = m?.kind === 'laravel'
  return [
    { id: 'fpm' as Rt, label: 'PHP-FPM', hint: 'Standard and most compatible. Uses the server\'s PHP version.', disabled: false, why: undefined },
    { id: 'frankenphp' as Rt, label: 'FrankenPHP', hint: 'Modern app server (classic mode). Any PHP app.', disabled: false, why: undefined },
    { id: 'octane' as Rt, label: 'Laravel Octane', hint: 'Worker mode: the app stays in memory — fastest. Needs laravel/octane.', disabled: !laravel, why: laravel ? undefined : 'Only for Laravel sites' },
  ]
})

// PHP switch
const installed = computed(() => (props.php?.versions ?? []).filter((v) => v.fpm_installed).map((v) => v.version))
const phpTarget = ref('')
watch(installed, (v) => {
  if (!phpTarget.value) phpTarget.value = v.find((x) => x !== props.site.php_version) ?? v[0] ?? ''
}, { immediate: true })

// SSL
const email = ref('')

// Settings
const opts = ref({ client_max_body_size: '64M', security_headers: true, hsts: false, cache_static: true, block_hidden: true })
const aliasText = ref('')

// Raw nginx editor
const raw = ref<{ path: string; real_path: string; content: string } | null>(null)
const rawText = ref('')
const rawErr = ref<string | null>(null)
async function openNginx() {
  tab.value = tab.value === 'nginx' ? '' : 'nginx'
  if (tab.value !== 'nginx' || raw.value) return
  try {
    raw.value = await api.get(`/api/nginx/file?path=${encodeURIComponent(props.site.config_path)}`)
    rawText.value = raw.value!.content
  } catch (e) { rawErr.value = errorMessage(e) }
}

function done(s: string) {
  if (s === 'applied') {
    emit('changed')
    raw.value = null
    loadModel()
  }
}
</script>

<template>
  <div class="manage stack" style="gap: 12px">
    <div class="row wrap" style="gap: 8px">
      <span class="tag" :style="managed ? 'border-color: var(--accent); color: var(--accent)' : ''">
        {{ managed ? 'Managed by NexaCloud' : 'Existing config (unmanaged)' }}
      </span>
      <button v-if="!managed && (site.root || site.proxy_pass)" type="button" class="btn btn-s btn-p" @click="tab = tab === 'adopt' ? '' : 'adopt'">
        <AppIcon name="check" :size="14" :stroke="2" /> Manage with NexaCloud
      </button>
      <button v-if="site.php_version" type="button" class="btn btn-s" :class="{ 'btn-on': tab === 'php' }" @click="tab = tab === 'php' ? '' : 'php'">
        <AppIcon name="php" :size="14" :stroke="2" /> Switch PHP
      </button>
      <button v-if="managed && !site.ssl" type="button" class="btn btn-s" :class="{ 'btn-on': tab === 'ssl' }" @click="tab = tab === 'ssl' ? '' : 'ssl'">
        <AppIcon name="ssl" :size="14" :stroke="2" /> Enable HTTPS
      </button>
      <button v-if="managed && model && (model.kind === 'laravel' || model.kind === 'php')" type="button" class="btn btn-s" :class="{ 'btn-on': tab === 'runtime' }" @click="tab = tab === 'runtime' ? '' : 'runtime'">
        <AppIcon name="services" :size="14" :stroke="2" /> Runtime: {{ model.runtime === 'frankenphp' ? 'FrankenPHP' : model.runtime === 'octane' ? 'Octane' : 'PHP-FPM' }}
      </button>
      <button v-if="managed && model && model.php_version && !model.user && model.runtime !== 'frankenphp'" type="button" class="btn btn-s" :class="{ 'btn-on': tab === 'isolate' }" @click="tab = tab === 'isolate' ? '' : 'isolate'">
        <AppIcon name="lock" :size="14" :stroke="2" /> Own user &amp; PHP pool
      </button>
      <span v-if="model?.user" class="tag mono" title="Isolated: own Linux user and PHP-FPM pool">user {{ model.user }}</span>
      <button v-if="managed" type="button" class="btn btn-s" :class="{ 'btn-on': tab === 'settings' }" @click="tab = tab === 'settings' ? '' : 'settings'">
        <AppIcon name="edit" :size="14" :stroke="2" /> Settings
      </button>
      <button type="button" class="btn btn-s" :class="{ 'btn-on': tab === 'nginx' }" @click="openNginx">
        <AppIcon name="file" :size="14" :stroke="2" /> Edit nginx config
      </button>
      <button v-if="managed" type="button" class="btn btn-s" :class="{ 'btn-on': tab === 'remove' }" @click="tab = tab === 'remove' ? '' : 'remove'">
        <AppIcon name="trash" :size="14" :stroke="2" /> Remove
      </button>
    </div>

    <div v-if="tab === 'php'" class="stack" style="gap: 10px">
      <p class="small muted">Now on PHP {{ site.php_version }}. NexaCloud switches the site's PHP-FPM socket, checks nginx and the site, and rolls back if anything fails.</p>
      <label class="row wrap" style="gap: 10px">
        <span class="small">Switch to</span>
        <SearchSelect v-model="phpTarget" :options="installed.map((v) => ({ value: v, label: `PHP ${v}`, disabled: v === site.php_version, hint: v === site.php_version ? 'current' : undefined }))" placeholder="Choose PHP version…" aria-label="Switch to PHP version" style="width: 220px" />
      </label>
      <PlanAction
        :key="`php-${phpTarget}`"
        :request="{ kind: 'site_php', params: { id: site.id, version: phpTarget } }"
        label="Preview switch"
        :disabled="!phpTarget || phpTarget === site.php_version"
        :success-text="`${site.id} now runs on PHP ${phpTarget}.`"
        @done="done"
      />
    </div>

    <div v-if="tab === 'adopt'" class="stack" style="gap: 10px">
      <p class="small text-2">NexaCloud rebuilds this site's nginx config from what it reads (domains, web root, PHP version, certificate), keeps your original file disabled on disk, and checks the site still answers — if not, everything is put back automatically.
        Afterwards you get HTTPS, settings, FrankenPHP and one-click PHP switching for this site too.</p>
      <label class="small"><input v-model="acceptCustom" type="checkbox"> I've read the list of custom rules (if any) and will re-add them in the site's custom.conf</label>
      <PlanAction
        :key="`adopt-${acceptCustom}`"
        :request="{ kind: 'site_adopt', params: { id: site.id, accept_custom: acceptCustom } }"
        label="Preview"
        apply-label="Manage this site"
        success-text="This site is now managed by NexaCloud."
        @done="done"
      />
    </div>

    <div v-if="tab === 'isolate'" class="stack" style="gap: 10px">
      <p class="small text-2">Runs this site as its own Linux user with its own PHP-FPM pool, and makes its .env readable by that user only. If another site on this server gets hacked, it can no longer read this site's files or database password. Deploys, queue workers and cron move to the new user. NexaCloud checks the site afterwards and undoes everything if it breaks.</p>
      <PlanAction
        :request="{ kind: 'site_isolate', params: { id: site.id } }"
        label="Preview"
        apply-label="Isolate this site"
        success-text="This site now runs as its own user."
        @done="done"
      />
    </div>

    <div v-if="tab === 'runtime' && model" class="stack" style="gap: 12px">
      <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 8px" role="radiogroup" aria-label="PHP runtime">
        <button v-for="r in runtimes" :key="r.id" type="button" role="radio" :aria-checked="runtimeTarget === r.id" :disabled="r.disabled" :title="r.why" :style="`min-height: 92px; padding: 12px; border-radius: 10px; cursor: ${r.disabled ? 'not-allowed' : 'pointer'}; display: flex; flex-direction: column; align-items: flex-start; gap: 6px; text-align: left; color: #E7ECF2; opacity: ${r.disabled ? .5 : 1}; background: ${runtimeTarget === r.id ? '#13211B' : '#0F1318'}; border: 1px solid ${runtimeTarget === r.id ? '#3DDC97' : '#2A3441'}`" @click="runtimeTarget = r.id">
          <span class="row" style="gap: 8px; font-size: 14px; font-weight: 600">{{ r.label }}<span v-if="model.runtime === r.id" class="pill ok">now</span></span>
          <span style="font-size: 12px; color: #A3AEBD; line-height: 1.4">{{ r.hint }}</span>
        </button>
      </div>
      <p v-if="runtimeTarget === 'octane'" class="small muted">
        <strong>Laravel Octane</strong> keeps your app booted in memory between requests (FrankenPHP worker mode) — often several times faster. Your project needs <span class="mono">laravel/octane</span>: run <span class="mono">composer require laravel/octane</span> and <span class="mono">php artisan octane:install --server=frankenphp</span>, commit and deploy first. Every deploy restarts the workers. Watch out for code that keeps state in static properties or singletons between requests.
      </p>
      <p v-else-if="runtimeTarget === 'frankenphp'" class="small muted">
        <strong>FrankenPHP</strong> (classic mode) is a modern PHP app server built on Caddy, run for this site on a private port; nginx keeps HTTPS and static files. It uses its own bundled PHP — check extensions with <span class="mono">frankenphp php-cli -m</span>.
      </p>
      <p v-else class="small muted"><strong>PHP-FPM</strong> with PHP {{ model.php_version }} — the standard setup. PHP limits and the PHP version switch apply here.</p>
      <p v-if="model.runtime !== 'fpm' && runtimeTarget !== 'fpm' && runtimeTarget !== model.runtime" class="small" style="color: var(--warn, #F7C55E)">Switch back to PHP-FPM first, then to the other runtime.</p>
      <PlanAction
        v-if="runtimeTarget !== model.runtime"
        :key="`rt-${runtimeTarget}`"
        :request="{ kind: 'site_runtime', params: { id: site.id, runtime: runtimeTarget } }"
        :label="`Preview switch to ${runtimes.find((r) => r.id === runtimeTarget)?.label}`"
        :success-text="`Now running on ${runtimes.find((r) => r.id === runtimeTarget)?.label}.`"
        @done="done"
      />
    </div>

    <div v-if="tab === 'ssl'" class="stack" style="gap: 10px">
      <p class="small muted">Gets a free Let's Encrypt certificate for {{ site.server_names.join(', ') }} and redirects HTTP to HTTPS. The domain must already point to this server. Renewal is automatic.</p>
      <label class="row wrap" style="gap: 10px">
        <span class="small">Email for expiry notices</span>
        <input v-model="email" class="in" type="email" placeholder="you@example.com" style="max-width: 300px">
      </label>
      <PlanAction
        :key="`ssl-${email}`"
        :request="{ kind: 'site_ssl', params: { id: site.id, email } }"
        label="Preview HTTPS"
        :disabled="!email.includes('@')"
        success-text="HTTPS is on."
        @done="done"
      />
    </div>

    <div v-if="tab === 'settings' && model" class="stack" style="gap: 10px">
      <label class="stack" style="gap: 6px">
        <span class="small">Extra domains (space separated)</span>
        <input v-model="aliasText" class="in mono" type="text" placeholder="www.example.com shop.example.com" spellcheck="false">
      </label>
      <div class="toggles">
        <label class="tog"><input v-model="opts.security_headers" type="checkbox"> Security headers <span class="muted small">X-Frame-Options, nosniff, Referrer-Policy</span></label>
        <label class="tog"><input v-model="opts.hsts" type="checkbox" :disabled="!model.ssl"> HSTS <span class="muted small">browsers always use HTTPS (needs SSL)</span></label>
        <label class="tog"><input v-model="opts.cache_static" type="checkbox"> Cache static files 30 days</label>
        <label class="tog"><input v-model="opts.block_hidden" type="checkbox"> Block hidden files (.env, .git)</label>
        <label class="tog">Max upload
          <SearchSelect v-model="opts.client_max_body_size" :options="['16M', '64M', '128M', '512M', '1G']" aria-label="Max upload size" style="width: 140px" />
        </label>
      </div>
      <PlanAction
        :key="JSON.stringify(opts) + aliasText"
        :request="{ kind: 'site_options', params: { id: site.id, aliases: aliasText.split(/[\s,]+/).filter(Boolean), options: opts } }"
        label="Preview changes"
        success-text="Settings saved."
        @done="done"
      />
    </div>

    <div v-if="tab === 'nginx'" class="stack" style="gap: 10px">
      <p v-if="rawErr" class="small" style="color: var(--bad)">{{ rawErr }}</p>
      <template v-if="raw">
        <p class="small muted">Editing <span class="mono">{{ raw.real_path }}</span>. Saved only if <span class="mono">nginx -t</span> passes — otherwise the old file is restored automatically.
          <template v-if="managed"> This file is regenerated by NexaCloud; put extra rules in <span class="mono">/etc/nginx/nexacloud/{{ site.id }}.custom.conf</span>.</template>
        </p>
        <textarea v-model="rawText" class="editor mono" spellcheck="false" rows="18" aria-label="nginx configuration" />
        <PlanAction
          :key="rawText.length + ':' + rawText.slice(-40)"
          :request="{ kind: 'nginx_raw', params: { path: raw.real_path, content: rawText } }"
          label="Preview & test"
          apply-label="Save and reload nginx"
          success-text="Saved and nginx reloaded."
          @done="done"
        />
      </template>
    </div>

    <div v-if="tab === 'remove'" class="stack" style="gap: 10px">
      <p class="small" style="color: var(--warn)">Removes the nginx config for {{ site.id }}. Website files and certificates stay on disk. You can undo it from Activity.</p>
      <PlanAction
        :request="{ kind: 'site_delete', params: { id: site.id } }"
        label="Preview removal"
        apply-label="Remove site"
        success-text="Site removed."
        @done="done"
      />
    </div>
  </div>
</template>

<style scoped>
.in { min-height: 40px; padding: 0 12px; background: var(--sunken, #0F1318); border: 1px solid var(--btn-line, #2A3441); border-radius: 8px; color: var(--text); font-size: 14px; box-sizing: border-box; }
.btn-on { border-color: var(--accent, #3DDC97); }
.toggles { display: grid; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr)); gap: 8px; }
.tog { display: flex; align-items: center; gap: 8px; font-size: 14px; flex-wrap: wrap; }
.tog input[type=checkbox] { width: 18px; height: 18px; accent-color: var(--accent, #3DDC97); }
.editor { width: 100%; box-sizing: border-box; background: #07090C; color: var(--text-2, #C5CEDA); border: 1px solid #1B222B; border-radius: 10px; padding: 14px; font-size: 12.5px; line-height: 1.6; resize: vertical; }
</style>
