<script setup lang="ts">
// Per-site actions shown in the expanded Sites row.
import type { PhpInfo, Site, SiteModel } from '~/types/api'

const props = defineProps<{ site: Site; php: PhpInfo | null }>()
const emit = defineEmits<{ changed: [] }>()
const api = useApi()

const managed = computed(() => props.site.config_path.startsWith('/etc/nginx/conf.d/nexacloud-'))
const model = ref<SiteModel | null>(null)
const tab = ref<'' | 'php' | 'ssl' | 'settings' | 'nginx' | 'remove' | 'runtime'>('')

async function loadModel() {
  if (!managed.value) return
  try {
    model.value = await api.get<SiteModel>(`/api/sites/${encodeURIComponent(props.site.id)}/model`)
    opts.value = { ...model.value.options }
    aliasText.value = model.value.domains.slice(1).join(' ')
  } catch { model.value = null }
}
onMounted(loadModel)

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
      <button v-if="site.php_version" type="button" class="btn btn-s" :class="{ 'btn-on': tab === 'php' }" @click="tab = tab === 'php' ? '' : 'php'">
        <AppIcon name="php" :size="14" :stroke="2" /> Switch PHP
      </button>
      <button v-if="managed && !site.ssl" type="button" class="btn btn-s" :class="{ 'btn-on': tab === 'ssl' }" @click="tab = tab === 'ssl' ? '' : 'ssl'">
        <AppIcon name="ssl" :size="14" :stroke="2" /> Enable HTTPS
      </button>
      <button v-if="managed && model && (model.kind === 'laravel' || model.kind === 'php')" type="button" class="btn btn-s" :class="{ 'btn-on': tab === 'runtime' }" @click="tab = tab === 'runtime' ? '' : 'runtime'">
        <AppIcon name="services" :size="14" :stroke="2" /> Runtime: {{ model.runtime === 'frankenphp' ? 'FrankenPHP' : 'PHP-FPM' }}
      </button>
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
        <select v-model="phpTarget" class="in" style="width: auto">
          <option v-for="v in installed" :key="v" :value="v" :disabled="v === site.php_version">PHP {{ v }}</option>
        </select>
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

    <div v-if="tab === 'runtime' && model" class="stack" style="gap: 10px">
      <p v-if="model.runtime !== 'frankenphp'" class="small muted">
        <strong>FrankenPHP</strong> is a modern PHP app server (built on Caddy). NexaCloud runs it for this site on a private port as its own service, and nginx keeps handling HTTPS and static files.
        It uses its own bundled PHP — check your extensions with <span class="mono">frankenphp php-cli -m</span> after switching. PHP-FPM stays installed, so switching back is one click.
      </p>
      <p v-else class="small muted">This site runs on FrankenPHP (port {{ model.runtime_port }}). Switch back to PHP {{ model.php_version }} FPM at any time.</p>
      <PlanAction
        :request="{ kind: 'site_runtime', params: { id: site.id, runtime: model.runtime === 'frankenphp' ? 'fpm' : 'frankenphp' } }"
        :label="model.runtime === 'frankenphp' ? 'Preview switch to PHP-FPM' : 'Preview switch to FrankenPHP'"
        :success-text="model.runtime === 'frankenphp' ? 'Back on PHP-FPM.' : 'Now running on FrankenPHP.'"
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
        <input v-model="aliasText" class="in mono" type="text" spellcheck="false">
      </label>
      <div class="toggles">
        <label class="tog"><input v-model="opts.security_headers" type="checkbox"> Security headers <span class="muted small">X-Frame-Options, nosniff, Referrer-Policy</span></label>
        <label class="tog"><input v-model="opts.hsts" type="checkbox" :disabled="!model.ssl"> HSTS <span class="muted small">browsers always use HTTPS (needs SSL)</span></label>
        <label class="tog"><input v-model="opts.cache_static" type="checkbox"> Cache static files 30 days</label>
        <label class="tog"><input v-model="opts.block_hidden" type="checkbox"> Block hidden files (.env, .git)</label>
        <label class="tog">Max upload
          <select v-model="opts.client_max_body_size" class="in" style="width: auto; min-height: 36px">
            <option>16M</option><option>64M</option><option>128M</option><option>512M</option><option>1G</option>
          </select>
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
