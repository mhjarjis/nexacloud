<script setup lang="ts">
// Board 05 · PHP & Services — same markup as the design, real data.
import type { CatalogItem, Job, PhpConfig, PhpInfo, Service, Site } from '~/types/api'

useHead({ title: 'PHP & Services · NexaCloud' })
const api = useApi()
const php = useResource<PhpInfo>('/api/php', { interval: 30_000 })
const sites = useResource<Site[]>('/api/sites')
const services = useResource<Service[]>('/api/services', { interval: 15_000 })
const catalog = useResource<CatalogItem[]>('/api/catalog')
const jobs = useResource<Job[]>('/api/jobs?limit=10', { interval: 4_000 })
const sys = useResource<{ hostname: string }>('/api/system')
const host = computed(() => sys.data.value?.hostname ?? 'server')

// ---- running job banner
const running = computed(() => (jobs.data.value ?? []).find((j) => j.status === 'running' || j.status === 'queued') ?? null)
const runningSteps = computed(() => (running.value?.log ?? '').split('\n').filter((l) => /\$ |Wrote |Verify:/.test(l)).slice(-5).map((l) => l.replace(/^\[[^\]]+\]\s*/, '').slice(0, 60)))
const runningFor = ref('0:00')
let tick: ReturnType<typeof setInterval> | null = null
onMounted(() => { tick = setInterval(() => { const s = running.value?.started_at; if (!s) return; const secs = Math.max(0, Math.round((Date.now() - new Date(s).getTime()) / 1000)); runningFor.value = `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, '0')}` }, 1000) })
onBeforeUnmount(() => { if (tick) clearInterval(tick) })
watch(running, (now, before) => { if (before && !now) { php.refresh(); services.refresh(); catalog.refresh() } })

// ---- PHP version cards
const KNOWN = ['8.5', '8.4', '8.3', '8.2', '8.1']
const cli = computed(() => php.data.value?.cli_default ?? null)
const phpCards = computed(() => {
  const vs = php.data.value?.versions ?? []
  const all = [...new Set([...KNOWN, ...vs.map((v) => v.version)])].sort((a, b) => Number(b) - Number(a))
  return all.map((v) => {
    const info = vs.find((x) => x.version === v)
    const inst = !!info?.fpm_installed
    const sup = info ? phpSupport(info) : null
    const siteList = (sites.data.value ?? []).filter((s) => s.php_version === v).map((s) => s.id.split('.')[0])
    const [state, cls] = !inst ? ['Not installed', 'off'] : sup?.tone === 'bad' ? ['End of life', 'bad'] : sup?.tone === 'warn' ? ['Ends soon', 'warn'] : ['Installed', 'ok']
    return {
      v, inst, state, cls,
      support: sup?.detail ?? (info?.eol ? 'No security fixes' : 'Available from ondrej/php'),
      sites: siteList.length ? `${siteList.length} site${siteList.length > 1 ? 's' : ''} · ${siteList.slice(0, 4).join(', ')}` : 'No sites yet',
      border: v === cli.value ? '#3DDC97' : '#222A34',
      isCli: v === cli.value,
    }
  })
})
const cardAction = ref<string | null>(null)

// ---- per-site PHP
const installedV = computed(() => (php.data.value?.versions ?? []).filter((v) => v.fpm_installed).map((v) => v.version))
const phpSites = computed(() => (sites.data.value ?? []).filter((s) => s.php_version))
const switchTo = reactive<Record<string, string>>({})
watch([phpSites, installedV], ([list]) => {
  for (const s of list) {
    if (!switchTo[s.id] || (switchTo[s.id] === s.php_version && installedV.value.length > 1)) switchTo[s.id] = installedV.value.filter((v) => v !== s.php_version).sort((a, b) => Number(b) - Number(a))[0] ?? s.php_version ?? ''
  }
}, { immediate: true })
const switching = ref<string | null>(null)
function compat(s: Site) {
  const info = php.data.value?.versions.find((v) => v.version === s.php_version)
  if (info?.eol) return { cls: 'bad', text: 'Current version is end-of-life' }
  return { cls: 'off', text: 'Checked on switch' }
}

// ---- extensions + php.ini
const extV = ref('')
watch(installedV, (v) => { if (!extV.value && v.length) extV.value = cli.value && v.includes(cli.value) ? cli.value : v[v.length - 1]! }, { immediate: true })
const cfg = ref<PhpConfig | null>(null)
const ini = ref<Record<string, string>>({})
const picked = ref<string[]>([])
async function loadCfg() {
  if (!extV.value) return
  cfg.value = null
  try {
    cfg.value = await api.get<PhpConfig>(`/api/php/${extV.value}/config`)
    ini.value = Object.fromEntries(['memory_limit', 'upload_max_filesize', 'max_execution_time', 'opcache.memory_consumption'].map((k) => [k, cfg.value!.ini[k] ?? '']))
    picked.value = []
  } catch { cfg.value = null }
}
watch(extV, loadCfg, { immediate: true })
const exts = computed(() => {
  const c = cfg.value
  if (!c) return []
  const names = [...new Set([...c.optional_extensions, ...c.loaded_modules.filter((m) => /^[a-z_]+$/.test(m) && m.length < 14)])].sort()
  return names.map((n) => ({ n, loaded: c.loaded_modules.includes(n) || c.loaded_modules.includes(n.replace(/^mysql$/, 'mysqli')), installable: c.optional_extensions.includes(n) }))
})
function toggleExt(n: string) { picked.value = picked.value.includes(n) ? picked.value.filter((x) => x !== n) : [...picked.value, n] }
const iniChanges = computed(() => {
  const out: Record<string, string> = {}
  for (const [k, v] of Object.entries(ini.value)) if ((cfg.value?.ini[k] ?? '') !== v) out[k] = v
  return out
})
const INI_HINTS: Record<string, string> = { memory_limit: '512M', upload_max_filesize: '64M', max_execution_time: '60', 'opcache.memory_consumption': '256' }

// ---- services
const TINTS: Record<string, string> = { nginx: '#5BE6A9', mysql: '#6CB4FF', mariadb: '#C5CEDA', postgresql: '#8CC4FF', redis: '#FF9A9A', 'redis-server': '#FF9A9A', supervisor: '#B79CFF', cron: '#F7C55E', fail2ban: '#F7C55E', ufw: '#FF9A9A', composer: '#F7C55E', nodejs: '#5BE6A9', clamav: '#F7C55E', certbot: '#5BE6A9', rclone: '#8CC4FF' }
const serviceCards = computed(() => {
  const svc = services.data.value ?? []
  const cat = catalog.data.value ?? []
  const out: { key: string; ab: string; name: string; ver: string; state: string; cls: string; note: string; tint: string; unit: string | null; installed: boolean; catalogId: string | null; active: boolean }[] = []
  for (const s of svc.filter((x) => x.installed && !/^php\d/.test(x.unit))) {
    const c = cat.find((x) => x.unit === s.unit)
    out.push({ key: s.unit, ab: (s.label || s.unit).replace(/[^A-Za-z0-9]/g, '').slice(0, 2).replace(/^./, (x) => x.toUpperCase()), name: s.label, ver: s.version ?? s.unit, state: s.active ? 'Running' : 'Stopped', cls: s.active ? 'ok' : 'bad', note: c?.description ?? `systemd unit ${s.unit}`, tint: TINTS[s.unit] ?? '#C5CEDA', unit: s.unit, installed: true, catalogId: null, active: s.active })
  }
  for (const c of cat.filter((x) => !out.some((o) => o.unit && o.unit === x.unit))) {
    out.push({ key: c.id, ab: c.label.replace(/[^A-Za-z0-9]/g, '').slice(0, 2).replace(/^./, (x) => x.toUpperCase()), name: c.label, ver: c.packages.join(', '), state: c.installed ? 'Installed' : 'Not installed', cls: c.installed ? 'ok' : 'off', note: c.description, tint: TINTS[c.id] ?? '#C5CEDA', unit: c.unit, installed: c.installed, catalogId: c.installed ? null : c.id, active: c.installed })
  }
  return out
})
const restarting = ref<string | null>(null)
const svcMsg = ref<Record<string, string>>({})
async function restart(unit: string, name = unit) {
  restarting.value = unit
  try { await api.post(`/api/services/${encodeURIComponent(unit)}/restart`); svcMsg.value[unit] = `${name} restarted.`; setTimeout(() => services.refresh(), 1500) } catch (e) { if (!(e instanceof SudoCancelled)) svcMsg.value[unit] = errorMessage(e) } finally { restarting.value = null }
}
const installing = ref<string | null>(null)
</script>

<template>
  <header style="display: flex; flex-wrap: wrap; align-items: flex-end; justify-content: space-between; gap: 16px">
    <div style="display: flex; flex-direction: column; gap: 6px">
      <p class="lbl">{{ host }} / PHP &amp; Services</p>
      <h1 style="font-size: 30px; font-weight: 600">PHP &amp; Services</h1>
    </div>
    <span style="font-size: 13px; color: #A3AEBD">Packages from ondrej/php, MySQL &amp; PGDG apt repositories</span>
  </header>

  <section v-if="running" class="card" aria-label="Running job" style="padding: 16px 20px; display: flex; flex-direction: column; gap: 12px; border-color: #24405A; background: #0F1822">
    <div style="display: flex; flex-wrap: wrap; align-items: center; gap: 12px">
      <span class="pill info">{{ running.status === 'queued' ? 'Queued' : 'Running' }}</span>
      <span style="font-size: 15px; font-weight: 600; flex: 1">{{ running.title }}</span>
      <span class="mono" style="font-size: 13px; color: #C5CEDA">{{ runningFor }}</span>
      <NuxtLink class="btn btn-s" :to="`/activity?open=${running.id}`">Show output</NuxtLink>
    </div>
    <div style="height: 6px; border-radius: 99px; background: #1B2633; overflow: hidden" role="progressbar" aria-label="Job running"><div class="indet" /></div>
    <div style="display: flex; flex-wrap: wrap; gap: 18px; font-size: 12px">
      <span v-for="(st, i) in runningSteps" :key="i" :style="`color: ${i === runningSteps.length - 1 ? '#8CC4FF' : '#5BE6A9'}`">{{ i === runningSteps.length - 1 ? '●' : '✓' }} {{ st }}</span>
    </div>
  </section>

  <section aria-labelledby="phpv" style="display: flex; flex-direction: column; gap: 12px">
    <div style="display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px">
      <h2 id="phpv" style="font-size: 18px; font-weight: 600">PHP versions</h2>
      <span style="font-size: 13px; color: #A3AEBD">CLI default: <span class="mono" style="color: #E7ECF2">php{{ cli ?? '—' }}</span> · use a card's button to make it the CLI default</span>
    </div>
    <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 12px">
      <div v-for="v in phpCards" :key="v.v" class="card" :style="`padding: 16px; display: flex; flex-direction: column; gap: 10px; border-color: ${v.border}`">
        <div style="display: flex; align-items: center; justify-content: space-between; gap: 8px">
          <span style="font-family: 'Space Grotesk', sans-serif; font-size: 26px; font-weight: 600">{{ v.v }}</span>
          <span :class="`pill ${v.cls}`">{{ v.state }}</span>
        </div>
        <span style="font-size: 13px; color: #C5CEDA">{{ v.support }}</span>
        <span style="font-size: 12px; color: #A3AEBD">{{ v.sites }}</span>
        <template v-if="cardAction === v.v">
          <PlanAction v-if="v.inst" :request="{ kind: 'php_cli', params: { version: v.v } }" label="Preview" apply-label="Make CLI default" :success-text="`php now runs PHP ${v.v}.`" @done="(s) => { if (s === 'applied') { cardAction = null; php.refresh() } }" />
          <PlanAction v-else :request="{ kind: 'php_install', params: { version: v.v } }" label="Preview install" apply-label="Install" :success-text="`PHP ${v.v} installed.`" @done="(s) => { if (s === 'applied') { cardAction = null; php.refresh() } }" />
        </template>
        <button v-else type="button" class="btn btn-s" :disabled="v.isCli" @click="cardAction = v.v">{{ v.isCli ? '✓ CLI default' : v.inst ? 'Make CLI default' : 'Install' }}</button>
      </div>
    </div>
  </section>

  <div style="display: flex; flex-wrap: wrap; gap: 16px; align-items: flex-start">
    <section class="card" aria-labelledby="per" style="flex: 3 1 520px; min-width: 0; overflow: hidden">
      <div style="padding: 16px 20px; display: flex; flex-direction: column; gap: 4px">
        <h2 id="per" style="font-size: 17px; font-weight: 600">PHP version per site</h2>
        <p style="font-size: 13px; color: #A3AEBD">Switching points the site at the new FPM pool, tests nginx, checks the site answers and rolls back automatically if it doesn't.</p>
      </div>
      <div style="overflow-x: auto">
        <table>
          <thead><tr><th>Site</th><th>Now</th><th>Switch to</th><th>Compatibility</th><th><span style="position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0)">Apply</span></th></tr></thead>
          <tbody>
            <template v-for="s in phpSites" :key="s.id">
              <tr>
                <td style="font-weight: 600">{{ s.id }}</td>
                <td><span class="tag mono">{{ s.php_version }}</span></td>
                <td><SearchSelect v-model="switchTo[s.id]" :options="installedV.map((v) => ({ value: v, label: v, disabled: v === s.php_version }))" mono :aria-label="`New PHP version for ${s.id}`" placeholder="version" style="width: 110px; min-width: 110px" /></td>
                <td><span :class="`pill ${compat(s).cls}`">{{ compat(s).text }}</span></td>
                <td><button type="button" class="btn btn-s" :disabled="!switchTo[s.id] || switchTo[s.id] === s.php_version" @click="switching = switching === s.id ? null : s.id">{{ switching === s.id ? 'Cancel' : 'Switch' }}</button></td>
              </tr>
              <tr v-if="switching === s.id">
                <td colspan="5" style="white-space: normal; background: #0F1318">
                  <PlanAction :request="{ kind: 'site_php', params: { id: s.id, version: switchTo[s.id] } }" label="Preview switch" apply-label="Switch" :success-text="`${s.id} now runs PHP ${switchTo[s.id]}.`" @done="(st) => { if (st === 'applied') { switching = null; sites.refresh(); php.refresh() } }" />
                </td>
              </tr>
            </template>
            <tr v-if="!phpSites.length"><td colspan="5" style="color: #A3AEBD">No PHP sites yet.</td></tr>
          </tbody>
        </table>
      </div>
    </section>

    <section class="card" aria-labelledby="ext" style="flex: 2 1 340px; min-width: 0; padding: 18px 20px; display: flex; flex-direction: column; gap: 14px">
      <div style="display: flex; align-items: center; justify-content: space-between; gap: 10px">
        <h2 id="ext" style="font-size: 17px; font-weight: 600">PHP {{ extV }} · extensions</h2>
        <SearchSelect v-model="extV" :options="installedV" mono aria-label="PHP version" placeholder="version" style="width: 100px; min-width: 100px" />
      </div>
      <div style="display: flex; flex-wrap: wrap; gap: 8px">
        <template v-for="e in exts" :key="e.n">
          <span v-if="e.loaded" class="pill ok mono" style="font-weight: 500">{{ e.n }}</span>
          <button v-else type="button" :class="`pill ${picked.includes(e.n) ? 'info' : 'off'} mono`" style="font-weight: 500; border: 0; cursor: pointer" :aria-pressed="picked.includes(e.n)" :title="`Install ${e.n}`" @click="toggleExt(e.n)">{{ picked.includes(e.n) ? '+ ' : '' }}{{ e.n }}</button>
        </template>
        <span v-if="!cfg" style="font-size: 13px; color: #A3AEBD">Loading…</span>
      </div>
      <PlanAction v-if="picked.length" :key="picked.join(',')" :request="{ kind: 'php_extensions', params: { version: extV, extensions: picked } }" :label="`Install ${picked.length} extension${picked.length > 1 ? 's' : ''}`" success-text="Extensions installed and PHP-FPM reloaded." @done="(s) => { if (s === 'applied') loadCfg() }" />
      <h3 style="font-size: 14px; font-weight: 600; margin-top: 4px">php.ini</h3>
      <div style="display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px">
        <label v-for="(_, k) in ini" :key="k" style="display: flex; flex-direction: column; gap: 6px"><span class="mono" style="font-size: 12px; color: #C5CEDA">{{ k }}</span><input v-model="ini[k]" class="in mono" type="text" :placeholder="INI_HINTS[k] ?? 'default'" style="font-size: 13px; width: 100%"></label>
      </div>
      <PlanAction :key="JSON.stringify(iniChanges)" :request="{ kind: 'php_ini', params: { version: extV, settings: iniChanges } }" label="Save & reload FPM" :disabled="!Object.keys(iniChanges).length" success-text="Saved and PHP-FPM reloaded." @done="(s) => { if (s === 'applied') loadCfg() }" />
    </section>
  </div>

  <section id="services" aria-labelledby="svc" style="display: flex; flex-direction: column; gap: 12px">
    <h2 id="svc" style="font-size: 18px; font-weight: 600">Databases, cache &amp; tools</h2>
    <div style="display: grid; grid-template-columns: repeat(auto-fill, minmax(250px, 1fr)); gap: 12px">
      <div v-for="s in serviceCards" :key="s.key" class="card" style="padding: 16px; display: flex; flex-direction: column; gap: 10px">
        <div style="display: flex; align-items: center; gap: 10px">
          <div :style="`width: 36px; height: 36px; border-radius: 9px; background: #1B222B; display: grid; place-items: center; font-family: 'Space Grotesk', sans-serif; font-weight: 700; font-size: 14px; color: ${s.tint}`" aria-hidden="true">{{ s.ab }}</div>
          <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px"><span style="font-size: 15px; font-weight: 600">{{ s.name }}</span><span class="mono svc-meta" style="font-size: 12px; color: #A3AEBD; overflow: hidden; text-overflow: ellipsis; white-space: nowrap">{{ s.ver }}</span></div>
          <span :class="`pill ${s.cls}`">{{ s.state }}</span>
        </div>
        <span style="font-size: 12px; color: #A3AEBD; line-height: 1.45">{{ s.note }}</span>
        <template v-if="s.catalogId">
          <PlanAction v-if="installing === s.catalogId" :request="{ kind: 'service_install', params: { id: s.catalogId } }" label="Preview install" apply-label="Install" :success-text="`${s.name} installed.`" @done="(st) => { if (st === 'applied') { installing = null; catalog.refresh(); services.refresh() } }" />
          <button v-else type="button" class="btn btn-s" style="flex: 1" @click="installing = s.catalogId">Install</button>
        </template>
        <div v-else-if="s.unit" style="display: flex; gap: 8px">
          <NuxtLink v-if="['mysql', 'mariadb', 'postgresql'].includes(s.unit)" class="btn btn-s" to="/databases" style="flex: 1">Manage</NuxtLink>
          <NuxtLink v-else-if="s.unit === 'nginx'" class="btn btn-s" to="/domains" style="flex: 1">Manage</NuxtLink>
          <NuxtLink v-else-if="['ufw', 'fail2ban'].includes(s.unit)" class="btn btn-s" to="/security" style="flex: 1">Manage</NuxtLink>
          <ConfirmAction label="Restart" :aria-label="`Restart ${s.name}`" :question="`Restart ${s.name}?`" confirm-label="Restart" :busy="restarting === s.unit" @confirm="restart(s.unit, s.name)" />
        </div>
        <span v-if="s.unit && svcMsg[s.unit]" class="small muted">{{ svcMsg[s.unit] }}</span>
      </div>
    </div>
  </section>
</template>

<style scoped>
.in { min-height: 40px; padding: 0 10px; box-sizing: border-box; }
.indet { width: 35%; height: 100%; background: #6CB4FF; border-radius: 99px; animation: indet 1.4s ease-in-out infinite; }
@keyframes indet { 0% { transform: translateX(-100%); } 100% { transform: translateX(300%); } }
</style>
