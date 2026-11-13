<script setup lang="ts">
// Board 04 · Domains, SSL & Nginx — same markup as the design, real data.
import type { PhpInfo, Site, SiteModel } from '~/types/api'

useHead({ title: 'Domains & SSL · NexaCloud' })
const api = useApi()
const route = useRoute()

interface Dom {
  domain: string; site_id: string; primary: boolean; kind: string; site_kind: string; serves: string | null
  php_version: string | null; ssl: boolean; ssl_expires: string | null; managed: boolean; project: boolean
  dns: { records: string[]; points_here: boolean; state: 'ok' | 'elsewhere' | 'missing' | 'unknown' }
}
// Fast list first (no DNS lookups), then the full check.
const quick = useResource<{ server_ips: string[]; domains: Dom[] }>('/api/domains?dns=false')
const full = useResource<{ server_ips: string[]; domains: Dom[] }>('/api/domains')
const data = {
  data: computed(() => full.data.value ?? quick.data.value),
  loading: computed(() => quick.loading.value && full.loading.value),
  refreshing: computed(() => full.refreshing.value || full.loading.value),
  refresh: () => { quick.refresh(); return full.refresh() },
}
const sites = useResource<Site[]>('/api/sites')
const php = useResource<PhpInfo>('/api/php')
const sys = useResource<{ hostname: string }>('/api/system')
const host = computed(() => sys.data.value?.hostname ?? 'server')

const ips = computed(() => (data.data.value?.server_ips ?? []).filter((ip) => !ip.startsWith('127.') && !ip.startsWith('172.1') && ip !== '::1' && !ip.startsWith('fe80')))
const ipv4 = computed(() => ips.value.find((ip) => ip.includes('.')) ?? '[server IPv4]')
async function copyRecords() { await navigator.clipboard?.writeText(`A  @  ${ipv4.value}\nA  *  ${ipv4.value}`) }

// ---- table
const q = ref(String(route.query.q ?? ''))
const domains = computed(() => (data.data.value?.domains ?? [])
  .filter((d) => !q.value.trim() || `${d.domain} ${d.serves ?? ''}`.toLowerCase().includes(q.value.trim().toLowerCase()))
  .map((d) => {
    const days = daysUntil(d.ssl_expires)
    return {
      ...d,
      name: d.domain,
      kindLabel: d.primary ? (d.kind === 'Proxy' ? 'Proxy' : d.domain.split('.').length > 2 && !d.domain.startsWith('www.') ? 'Subdomain' : 'Primary') : 'Alias',
      servesText: d.site_kind === 'proxy' ? `→ ${(d.serves ?? '').replace(/^https?:\/\//, '')}` : d.primary ? `${d.site_id.split('.')[0]} → ${(d.serves ?? '').replace(/^.*\/(current\/)?/, '/')}` : `alias of ${d.site_id}`,
      dnsText: d.dns.state === 'ok' ? 'Points here' : d.dns.state === 'elsewhere' ? 'Points elsewhere' : d.dns.state === 'unknown' ? 'Checking…' : 'Not pointing',
      dnsCls: d.dns.state === 'ok' ? 'ok' : d.dns.state === 'elsewhere' ? 'warn' : d.dns.state === 'unknown' ? 'info' : 'bad',
      sslText: !d.ssl ? (d.dns.state === 'ok' ? 'No HTTPS' : 'Waiting for DNS') : days === null ? 'Valid' : days < 0 ? 'Expired' : days < 30 ? `Renews in ${days} d` : `Valid · ${days} d`,
      sslCls: !d.ssl ? (d.dns.state === 'ok' ? 'off' : 'warn') : days !== null && days < 0 ? 'bad' : days !== null && days < 14 ? 'warn' : 'ok',
      phpText: d.php_version ?? '—',
    }
  }))
const manage = ref<{ site: string; tab: string } | null>(null)
const manageSite = computed(() => sites.data.value?.find((s) => s.id === manage.value?.site) ?? null)
function openManage(site: string, tab = '') { manage.value = manage.value?.site === site && manage.value.tab === tab ? null : { site, tab } }
function refreshAll() { data.refresh(); sites.refresh(); loadNginx() }

// ---- add domain (full form)
const adding = ref(false)

// ---- add subdomain
const sub = ref('')
const apexes = computed(() => [...new Set((data.data.value?.domains ?? []).map((d) => d.domain.replace(/^www\./, '').split('.').slice(-2).join('.')))].sort())
const base = ref('')
watch(apexes, (a) => { if (!base.value && a.length) base.value = a[0]! }, { immediate: true })
const target = ref<'project' | 'redirect' | 'proxy'>('project')
const targets = computed(() => ([['project', 'Project', 'Serve a site'], ['redirect', 'Redirect', '301 / 302'], ['proxy', 'Proxy', 'Local port']] as const).map(([id, label, hint]) => ({
  id, label, hint, pressed: target.value === id, bg: target.value === id ? '#13211B' : '#0F1318', border: target.value === id ? '#3DDC97' : '#2A3441',
})))
const subProject = ref('new:laravel')
const projectOptions = computed(() => [
  { value: 'new:laravel', label: 'New empty Laravel project' },
  { value: 'new:php', label: 'New empty PHP project' },
  { value: 'new:static', label: 'New static site' },
  ...(sites.data.value ?? []).filter((s) => s.root && s.config_path.includes('nexacloud-')).map((s) => ({ value: `alias:${s.id}`, label: `${s.id}`, hint: 'same site, extra domain' })),
])
const redirectTo = ref('')
const redirectCode = ref(301)
const proxyPort = ref('127.0.0.1:8080')
const subSsl = ref(false)
const subEmail = ref('')
const subPhp = ref('')
watch(() => php.data.value, (p) => { if (p && !subPhp.value) subPhp.value = p.versions.filter((v) => v.fpm_installed).map((v) => v.version).pop() ?? '' }, { immediate: true })
const fqdn = computed(() => (sub.value.trim() && base.value ? `${sub.value.trim().toLowerCase()}.${base.value}` : ''))
const subRequest = computed(() => {
  if (target.value === 'project' && subProject.value.startsWith('alias:')) {
    const sid = subProject.value.slice(6)
    const s = sites.data.value?.find((x) => x.id === sid)
    return { kind: 'site_options', params: { id: sid, aliases: [...(s?.server_names.slice(1) ?? []), fqdn.value], options: siteOptions.value[sid] ?? undefined } }
  }
  const k = target.value === 'redirect' ? 'redirect' : target.value === 'proxy' ? 'proxy' : subProject.value.slice(4)
  return {
    kind: 'site_create',
    params: {
      kind: k, domain: fqdn.value, aliases: [],
      php_version: ['laravel', 'php'].includes(k) ? subPhp.value : null,
      upstream: k === 'proxy' ? `http://${proxyPort.value.replace(/^https?:\/\//, '')}` : null,
      redirect_to: k === 'redirect' ? redirectTo.value.trim() : null,
      redirect_code: k === 'redirect' ? redirectCode.value : null,
    },
  }
})
const siteOptions = ref<Record<string, unknown>>({})
watch(subProject, async (v) => {
  if (v.startsWith('alias:')) {
    const sid = v.slice(6)
    try { siteOptions.value[sid] = (await api.get<SiteModel>(`/api/sites/${encodeURIComponent(sid)}/model`)).options } catch { /* unmanaged */ }
  }
})
const subOk = computed(() => /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/.test(sub.value.trim().toLowerCase()) && !!base.value && (target.value !== 'redirect' || /^https?:\/\/.+/.test(redirectTo.value.trim())) && (!subSsl.value || /.+@.+\..+/.test(subEmail.value)))
const sslAfter = ref<string | null>(null)
function subDone(s: string) {
  if (s !== 'applied') return
  if (subSsl.value) sslAfter.value = subProject.value.startsWith('alias:') ? subProject.value.slice(6) : fqdn.value
  refreshAll()
}

// ---- nginx editor
const ngxSite = ref('')
watch(() => sites.data.value, (s) => { if (s && !ngxSite.value) ngxSite.value = s.find((x) => x.config_path.includes('nexacloud-'))?.id ?? s[0]?.id ?? '' }, { immediate: true })
const ngxSiteObj = computed(() => sites.data.value?.find((s) => s.id === ngxSite.value) ?? null)
const ngxManaged = computed(() => !!ngxSiteObj.value?.config_path.includes('nexacloud-'))
const mode = ref<'visual' | 'raw'>('visual')
watch(ngxManaged, (m) => { if (!m) mode.value = 'raw' })
const modes = computed(() => ([['visual', 'Visual'], ['raw', 'Raw config']] as const).map(([id, label]) => ({ id, label, pressed: mode.value === id, bg: mode.value === id ? '#1F2833' : 'transparent', fg: mode.value === id ? '#E7ECF2' : '#A3AEBD', disabled: id === 'visual' && !ngxManaged.value })))
const model = ref<SiteModel | null>(null)
const opts = ref<Record<string, unknown> & { client_max_body_size: string }>({ client_max_body_size: '64M' })
const raw = ref<{ path: string; real_path: string; content: string } | null>(null)
const rawText = ref('')
async function loadNginx() {
  if (!ngxSiteObj.value) return
  model.value = null; raw.value = null
  if (ngxManaged.value) {
    try { model.value = await api.get<SiteModel>(`/api/sites/${encodeURIComponent(ngxSite.value)}/model`); opts.value = { ...(model.value.options as unknown as Record<string, unknown>) } as typeof opts.value } catch { model.value = null }
  }
  try { raw.value = await api.get(`/api/nginx/file?path=${encodeURIComponent(ngxSiteObj.value.config_path)}`); rawText.value = raw.value!.content } catch { raw.value = null }
}
watch(ngxSite, loadNginx)
watch(ngxSiteObj, (v, old) => { if (v && !old) loadNginx() })
const OPTS = [
  { key: 'security_headers', name: 'Security headers', hint: 'X-Frame-Options, nosniff, Referrer-Policy' },
  { key: 'hsts', name: 'HSTS', hint: 'Browsers always use HTTPS (needs SSL)' },
  { key: 'cache_static', name: 'Cache static assets', hint: 'expires 30d on css, js, images, fonts' },
  { key: 'block_hidden', name: 'Block hidden files', hint: 'Deny /.env, /.git' },
  { key: 'front_controller', name: 'Front controller', hint: 'Unknown paths go to index.php' },
  { key: 'maintenance', name: 'Maintenance mode', hint: '503 “back soon” page for visitors' },
]
const visualRequest = computed(() => ({ kind: 'site_options', params: { id: ngxSite.value, options: opts.value } }))
const siteOptionsList = computed(() => (sites.data.value ?? []).map((s) => ({ value: s.id, label: s.id, hint: s.config_path.includes('nexacloud-') ? undefined : 'existing config' })))
</script>

<template>
  <header style="display: flex; flex-wrap: wrap; align-items: flex-end; justify-content: space-between; gap: 16px">
    <div style="display: flex; flex-direction: column; gap: 6px">
      <p class="lbl">{{ host }} / Domains &amp; SSL</p>
      <h1 style="font-size: 30px; font-weight: 600">Domains, subdomains &amp; SSL</h1>
    </div>
    <div style="display: flex; flex-wrap: wrap; gap: 10px">
      <button type="button" class="btn" :disabled="data.refreshing.value" @click="data.refresh()">Re-check all DNS</button>
      <button type="button" class="btn btn-p" @click="adding = !adding">Add domain</button>
    </div>
  </header>

  <SiteCreate v-if="adding" title="Add a domain" @done="adding = false; refreshAll()" @close="adding = false" />

  <section class="card" aria-label="DNS instructions" style="padding: 16px 20px; display: flex; flex-wrap: wrap; align-items: center; gap: 16px">
    <div style="flex: 1 1 320px; display: flex; flex-direction: column; gap: 4px">
      <span style="font-size: 15px; font-weight: 600">Point a domain to this server</span>
      <span style="font-size: 13px; color: #A3AEBD">At your registrar (or Hostinger DNS Manager) add these records. The DNS column below shows whether each domain already points here; HTTPS can be issued once it does.</span>
    </div>
    <div class="mono" style="display: grid; grid-template-columns: repeat(3, auto); gap: 6px 20px; font-size: 13px; padding: 12px 16px; background: #0F1318; border-radius: 10px; border: 1px solid #222A34">
      <span style="color: #A3AEBD">Type</span><span style="color: #A3AEBD">Name</span><span style="color: #A3AEBD">Value</span>
      <span>A</span><span>@</span><span>{{ ipv4 }}</span>
      <span>A</span><span>*</span><span>{{ ipv4 }}</span>
    </div>
    <button type="button" class="btn btn-s" @click="copyRecords">Copy records</button>
  </section>

  <section class="card" aria-labelledby="dl" style="overflow: hidden">
    <div style="display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px; padding: 16px 20px">
      <h2 id="dl" style="font-size: 17px; font-weight: 600">{{ data.loading.value && !data.data.value ? 'Checking domains and DNS…' : `${domains.length} domains` }}</h2>
      <input v-model="q" class="in" type="search" aria-label="Filter domains" placeholder="Filter domains" style="max-width: 260px; width: 100%">
    </div>
    <div style="overflow-x: auto">
      <table>
        <thead><tr><th>Domain</th><th>Kind</th><th>Serves</th><th>DNS</th><th>SSL</th><th>PHP</th><th><span style="position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0)">Actions</span></th></tr></thead>
        <tbody>
          <template v-for="d in domains" :key="d.name">
            <tr>
              <td style="font-weight: 600">{{ d.name }}<div v-if="d.primary && !d.managed" style="font-size: 12px; font-weight: 400; color: #F7C55E">existing config</div></td>
              <td><span class="tag">{{ d.kindLabel }}</span></td>
              <td class="mono" style="font-size: 13px; color: #C5CEDA">{{ d.servesText }}</td>
              <td><span :class="`pill ${d.dnsCls}`" :title="d.dns.records.join(', ') || 'no A record'">{{ d.dnsText }}</span></td>
              <td>
                <button v-if="!d.ssl && d.primary && d.managed" type="button" :class="`pill ${d.sslCls}`" style="border: 0; cursor: pointer" :title="'Enable HTTPS for ' + d.name" @click="openManage(d.site_id, 'ssl')">{{ d.sslText }} · Enable</button>
                <span v-else :class="`pill ${d.sslCls}`">{{ d.sslText }}</span>
              </td>
              <td class="mono" style="font-size: 13px">{{ d.phpText }}</td>
              <td><div style="display: flex; gap: 6px"><button type="button" class="btn btn-s" @click="ngxSite = d.site_id; mode = ngxManaged ? mode : 'raw'">Nginx</button><button type="button" class="btn btn-s" :aria-label="`More actions for ${d.name}`" :aria-expanded="manage?.site === d.site_id" @click="openManage(d.site_id)">···</button></div></td>
            </tr>
            <tr v-if="manage?.site === d.site_id && d.primary && manageSite">
              <td colspan="7" style="white-space: normal; background: #0F1318; padding: 16px 20px">
                <SiteManage :key="`${manage.site}-${manage.tab}`" :site="manageSite" :php="php.data.value" :initial-tab="manage.tab" @changed="refreshAll" />
              </td>
            </tr>
          </template>
          <tr v-if="!domains.length && !data.loading.value"><td colspan="7" style="color: #A3AEBD">No domains match.</td></tr>
        </tbody>
      </table>
    </div>
  </section>

  <div style="display: flex; flex-wrap: wrap; gap: 16px; align-items: flex-start">
    <section class="card" aria-labelledby="sub" style="flex: 1 1 360px; min-width: 0; padding: 20px; display: flex; flex-direction: column; gap: 14px">
      <h2 id="sub" style="font-size: 17px; font-weight: 600">Add subdomain</h2>
      <div style="display: flex; align-items: flex-end; gap: 0">
        <label class="field" style="flex: 1"><span>Subdomain</span><input v-model="sub" class="in" type="text" placeholder="staging" style="border-radius: 8px 0 0 8px" autocomplete="off" spellcheck="false"></label>
        <SearchSelect v-model="base" :options="apexes" allow-custom mono placeholder=".example.com" aria-label="Base domain" style="width: 180px" />
      </div>
      <fieldset style="border: 0; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 8px">
        <legend style="font-size: 13px; font-weight: 500; color: #C5CEDA; margin-bottom: 8px">Points to</legend>
        <div style="display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 8px">
          <button v-for="t in targets" :key="t.id" type="button" :aria-pressed="t.pressed" :style="`min-height: 64px; padding: 10px; border-radius: 10px; cursor: pointer; text-align: left; display: flex; flex-direction: column; gap: 4px; background: ${t.bg}; border: 1px solid ${t.border}; color: #E7ECF2`" @click="target = t.id"><span style="font-size: 13px; font-weight: 600">{{ t.label }}</span><span style="font-size: 11px; color: #A3AEBD">{{ t.hint }}</span></button>
        </div>
      </fieldset>
      <template v-if="target === 'project'">
        <label class="field"><span>Project</span><SearchSelect v-model="subProject" :options="projectOptions" placeholder="Choose…" aria-label="Project" /></label>
        <label v-if="subProject === 'new:laravel' || subProject === 'new:php'" class="field"><span>PHP version</span><SearchSelect v-model="subPhp" :options="(php.data.value?.versions ?? []).filter((v) => v.fpm_installed).map((v) => v.version)" placeholder="PHP version" aria-label="PHP version" /></label>
      </template>
      <div v-if="target === 'redirect'" style="display: grid; grid-template-columns: 2fr 1fr; gap: 10px">
        <label class="field"><span>Redirect to</span><input v-model="redirectTo" class="in" type="text" placeholder="https://example.com"></label>
        <label class="field"><span>Code</span><SearchSelect v-model="redirectCode" :options="[{ value: 301, label: '301 permanent' }, { value: 302, label: '302 temporary' }]" aria-label="Redirect code" /></label>
      </div>
      <label v-if="target === 'proxy'" class="field"><span>Local port (Node, Reverb, Soketi…)</span><input v-model="proxyPort" class="in mono" type="text" placeholder="127.0.0.1:8080"></label>
      <label style="display: flex; align-items: center; gap: 10px; font-size: 14px"><input v-model="subSsl" type="checkbox" style="width: 18px; height: 18px; accent-color: #3DDC97">Issue SSL certificate</label>
      <input v-if="subSsl" v-model="subEmail" class="in" type="email" placeholder="Email for certificate notices" aria-label="Email for certificate notices">
      <label style="display: flex; align-items: center; gap: 10px; font-size: 14px; color: #6B7684" title="Coming soon"><input type="checkbox" disabled style="width: 18px; height: 18px">Password-protect (basic auth) · coming soon</label>
      <PlanAction :key="JSON.stringify(subRequest)" :request="subRequest" label="Create subdomain" apply-label="Create subdomain" :disabled="!subOk" :success-text="`${fqdn} created.`" @done="subDone" />
      <div v-if="sslAfter" style="display: flex; flex-direction: column; gap: 8px">
        <span style="font-size: 13px; color: #C5CEDA">Now the certificate for <span class="mono">{{ fqdn }}</span> (the domain must point here):</span>
        <PlanAction :request="{ kind: 'site_ssl', params: { id: sslAfter, email: subEmail } }" label="Preview HTTPS" success-text="HTTPS is on." @done="(s) => { if (s === 'applied') { sslAfter = null; refreshAll() } }" />
      </div>
    </section>

    <section class="card" aria-labelledby="ngx" style="flex: 2 1 560px; min-width: 0; padding: 20px; display: flex; flex-direction: column; gap: 14px">
      <div style="display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px">
        <div style="display: flex; flex-direction: column; gap: 4px; min-width: 0">
          <h2 id="ngx" style="font-size: 17px; font-weight: 600; display: flex; align-items: center; gap: 8px">Nginx · <SearchSelect v-model="ngxSite" :options="siteOptionsList" mono placeholder="Choose a site…" aria-label="Site" style="width: 240px" /></h2>
          <span class="mono" style="font-size: 12px; color: #A3AEBD">{{ raw?.real_path ?? ngxSiteObj?.config_path ?? '' }}</span>
        </div>
        <div role="group" aria-label="Editor mode" style="display: flex; gap: 4px; padding: 4px; background: #0F1318; border: 1px solid #222A34; border-radius: 10px">
          <button v-for="m in modes" :key="m.id" type="button" :aria-pressed="m.pressed" :disabled="m.disabled" :title="m.disabled ? 'Manage this site with NexaCloud to use the visual editor' : undefined" :style="`min-height: 34px; padding: 0 14px; border-radius: 7px; border: 0; cursor: pointer; font-size: 13px; font-weight: 600; background: ${m.bg}; color: ${m.fg}`" @click="mode = m.id">{{ m.label }}</button>
        </div>
      </div>

      <template v-if="mode === 'visual' && model">
        <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); gap: 10px">
          <label v-for="o in OPTS" :key="o.key" style="display: flex; align-items: center; gap: 12px; padding: 12px 14px; background: #0F1318; border: 1px solid #222A34; border-radius: 10px; cursor: pointer">
            <span style="flex: 1; display: flex; flex-direction: column; gap: 2px"><span style="font-size: 14px; font-weight: 600">{{ o.name }}</span><span style="font-size: 12px; color: #A3AEBD">{{ o.hint }}</span></span>
            <input v-model="opts[o.key]" type="checkbox" :disabled="o.key === 'hsts' && !model.ssl" style="width: 20px; height: 20px; accent-color: #3DDC97">
          </label>
          <label style="display: flex; align-items: center; gap: 12px; padding: 12px 14px; background: #0F1318; border: 1px solid #222A34; border-radius: 10px">
            <span style="flex: 1; display: flex; flex-direction: column; gap: 2px"><span style="font-size: 14px; font-weight: 600">Max upload size</span><span style="font-size: 12px; color: #A3AEBD">client_max_body_size</span></span>
            <SearchSelect v-model="opts.client_max_body_size" :options="['16M', '64M', '128M', '256M', '512M', '1G']" aria-label="Max upload size" style="width: 110px; min-width: 110px" />
          </label>
        </div>
        <div style="display: flex; flex-wrap: wrap; align-items: center; gap: 10px; padding-top: 4px">
          <span class="pill ok">nginx -t checked on save</span>
          <span style="font-size: 12px; color: #A3AEBD; flex: 1">Previous version saved. Roll back any time from Activity.</span>
        </div>
        <PlanAction :key="JSON.stringify(opts)" :request="visualRequest" label="Test config" apply-label="Save & reload" success-text="Saved and nginx reloaded." @done="(s) => { if (s === 'applied') refreshAll() }" />
      </template>
      <template v-else-if="mode === 'raw' && raw">
        <textarea v-model="rawText" class="mono" spellcheck="false" aria-label="nginx configuration" style="background: #07090C; border: 1px solid #1B222B; border-radius: 10px; padding: 16px; font-size: 12px; line-height: 1.75; color: #C5CEDA; min-height: 360px; resize: vertical; width: 100%; box-sizing: border-box" />
        <div style="display: flex; flex-wrap: wrap; align-items: center; gap: 10px; padding-top: 4px">
          <span class="pill ok">Saved only if nginx -t passes</span>
          <span style="font-size: 12px; color: #A3AEBD; flex: 1">{{ ngxManaged ? 'This file is regenerated by NexaCloud — put extra rules in the site’s custom.conf.' : 'Previous version saved. Roll back any time.' }}</span>
        </div>
        <PlanAction :key="rawText.length + rawText.slice(-40)" :request="{ kind: 'nginx_raw', params: { path: raw.real_path, content: rawText } }" label="Test config" apply-label="Save & reload" success-text="Saved and nginx reloaded." @done="(s) => { if (s === 'applied') refreshAll() }" />
      </template>
      <p v-else style="font-size: 13px; color: #A3AEBD">Choose a site to edit its nginx config.</p>
    </section>
  </div>
</template>

<style scoped>
.field { display: flex; flex-direction: column; gap: 6px; }
.field > span:first-child { font-size: 13px; font-weight: 500; color: #C5CEDA; }
.in { min-height: 44px; padding: 0 12px; box-sizing: border-box; }
</style>
