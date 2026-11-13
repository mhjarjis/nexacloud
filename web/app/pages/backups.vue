<script setup lang="ts">
import type { BackupDestination, BackupPlan, BackupRun, DatabasesInfo, DiskUsageItem } from '~/types/api'

useHead({ title: 'Backups & Storage · NexaCloud' })

const api = useApi()
const dests = useResource<BackupDestination[]>('/api/backup/destinations')
const plans = useResource<BackupPlan[]>('/api/backup/plans')
const runs = useResource<BackupRun[]>('/api/backup/runs?limit=30', { interval: 15_000 })
const dbs = useResource<DatabasesInfo>('/api/databases')
const disk = useResource<DiskUsageItem[]>('/api/disk')
const sys = useResource<import('~/types/api').SystemInfo>('/api/system')

const err = ref<string | null>(null)
const jobs = reactive<Record<string, string>>({}) // key → job id being watched

// ---------------------------------------------------------------- destinations
const addingDest = ref(false)
const dKind = ref<'sftp' | 's3' | 'local'>('sftp')
const dName = ref('Synology NAS')
const sftp = reactive({ host: '', port: 22, user: 'backup', path: '/volume1/backups/vps' })
const S3_PRESETS = [
  { id: 'Synology', label: 'Synology C2', endpoint: 'https://eu-002.s3.synologyc2.net', region: 'eu-002' },
  { id: 'AWS', label: 'Amazon S3', endpoint: '', region: 'eu-central-1' },
  { id: 'Minio', label: 'MinIO (on your NAS)', endpoint: 'http://nas:9000', region: '' },
  { id: 'Other', label: 'Backblaze B2 / other', endpoint: 'https://s3.eu-central-003.backblazeb2.com', region: '' },
]
const s3 = reactive({ provider: 'Synology', endpoint: S3_PRESETS[0]!.endpoint, region: 'eu-002', bucket: '', access_key: '', secret_key: '', path: 'vps' })
const local = reactive({ path: '/var/backups/offsite' })
const encrypt = ref(true)
const encPass = ref('')
function genPass() {
  const a = new Uint8Array(18); crypto.getRandomValues(a)
  encPass.value = btoa(String.fromCharCode(...a)).replace(/[+/=]/g, '').slice(0, 24)
}
const pubKey = ref<string | null>(null)
watch(dKind, async (k) => {
  dName.value = k === 'sftp' ? 'Synology NAS' : k === 's3' ? 'Cloud storage' : 'Second disk'
  if (k === 'sftp' && !pubKey.value) {
    try { pubKey.value = (await api.get<{ public_key: string }>('/api/backup/ssh-key')).public_key } catch (e) { err.value = errorMessage(e) }
  }
}, { immediate: true })
function preset(id: string) {
  const p = S3_PRESETS.find((x) => x.id === id)!
  s3.provider = id; s3.endpoint = p.endpoint; s3.region = p.region
}
async function addDest() {
  err.value = null
  const config: Record<string, unknown> = dKind.value === 'sftp' ? { ...sftp } : dKind.value === 's3' ? { ...s3 } : { ...local }
  if (dKind.value !== 'local' && encrypt.value) {
    if (encPass.value.length < 12) { err.value = 'Use an encryption password of at least 12 characters (or press Generate).'; return }
    config.encrypt_password = encPass.value
  }
  try {
    await api.post('/api/backup/destinations', { name: dName.value, kind: dKind.value, config })
    addingDest.value = false
    dests.refresh()
  } catch (e) { err.value = errorMessage(e) }
}
async function testDest(d: BackupDestination) {
  try { jobs[`test-${d.id}`] = (await api.post<{ job_id: string }>(`/api/backup/destinations/${d.id}/test`)).job_id } catch (e) { err.value = errorMessage(e) }
}
async function delDest(d: BackupDestination) {
  try { await api.del(`/api/backup/destinations/${d.id}`); dests.refresh() } catch (e) { err.value = errorMessage(e) }
}
async function copyKey() { if (pubKey.value) await navigator.clipboard?.writeText(pubKey.value) }

// ---------------------------------------------------------------- plans
const editing = ref<string | null>(null) // plan id or 'new'
const form = reactive({ name: 'Nightly', destination_id: '' as string, allDbs: true, dbs: [] as string[], paths: '', freq: 'daily', time: '03:00', day: 'sun', keep_daily: 7, keep_weekly: 4, keep_monthly: 3, enabled: true })
const dbChoices = computed(() => {
  const d = dbs.data.value
  if (!d) return []
  return [
    ...(d.mysql.available ? d.mysql.databases.map((x) => `mysql/${x.name}`) : []),
    ...(d.postgres.available ? d.postgres.databases.map((x) => `postgres/${x.name}`) : []),
  ]
})
function schedule() {
  return form.freq === 'daily' ? `daily ${form.time}` : form.freq === 'weekly' ? `weekly ${form.day} ${form.time}` : form.freq
}
function scheduleText(s: string) {
  const p = s.split(' ')
  if (p[0] === 'daily') return `Every day at ${p[1]} UTC`
  if (p[0] === 'weekly') return `Every ${p[1]} at ${p[2]} UTC`
  if (p[0] === 'hourly') return 'Every hour'
  return 'Only when you click Run'
}
function edit(p?: BackupPlan) {
  editing.value = p?.id ?? 'new'
  form.name = p?.name ?? 'Nightly'
  form.destination_id = p?.destination_id ?? (dests.data.value?.[0]?.id ?? '')
  form.allDbs = p ? p.databases.includes('all') : true
  form.dbs = p ? p.databases.filter((d) => d !== 'all') : []
  form.paths = p ? p.paths.join('\n') : ''
  const s = (p?.schedule ?? 'daily 03:00').split(' ')
  form.freq = s[0]!; form.time = s[s.length - 1]!.includes(':') ? s[s.length - 1]! : '03:00'; form.day = s[0] === 'weekly' ? s[1]! : 'sun'
  form.keep_daily = p?.keep_daily ?? 7; form.keep_weekly = p?.keep_weekly ?? 4; form.keep_monthly = p?.keep_monthly ?? 3
  form.enabled = p?.enabled ?? true
}
async function savePlan() {
  err.value = null
  const body = {
    name: form.name, destination_id: form.destination_id || null,
    databases: form.allDbs ? ['all'] : form.dbs,
    paths: form.paths.split('\n').map((x) => x.trim()).filter(Boolean),
    schedule: schedule(), keep_daily: form.keep_daily, keep_weekly: form.keep_weekly, keep_monthly: form.keep_monthly, enabled: form.enabled,
  }
  try {
    if (editing.value === 'new') await api.post('/api/backup/plans', body)
    else await api.put(`/api/backup/plans/${editing.value}`, body)
    editing.value = null
    plans.refresh()
  } catch (e) { err.value = errorMessage(e) }
}
async function delPlan(p: BackupPlan) {
  try { await api.del(`/api/backup/plans/${p.id}`); plans.refresh() } catch (e) { err.value = errorMessage(e) }
}
async function runPlan(p: BackupPlan) {
  try { jobs[`run-${p.id}`] = (await api.post<{ job_id: string }>(`/api/backup/plans/${p.id}/run`)).job_id; setTimeout(() => runs.refresh(), 800) } catch (e) { err.value = errorMessage(e) }
}
const destName = (id: string | null) => (id ? dests.data.value?.find((d) => d.id === id)?.name ?? 'deleted destination' : 'This server only')
const planName = (id: string) => plans.data.value?.find((p) => p.id === id)?.name ?? '—'

// ---------------------------------------------------------------- staging
const staging = reactive<Record<string, { files: string; database: string; domain: string } | undefined>>({})
const stagingMsg = reactive<Record<string, string>>({})
function openStaging(r: BackupRun) {
  if (staging[r.id]) { staging[r.id] = undefined; return }
  const files = r.items.find((i) => i.kind === 'files')?.source ?? ''
  const base = files.split('/').filter(Boolean).pop() ?? 'site'
  staging[r.id] = { files, database: r.items.find((i) => i.kind === 'database')?.source ?? '', domain: `staging.${base.includes('.') ? base : base + '.example.com'}` }
}
async function startStaging(r: BackupRun) {
  const f = staging[r.id]
  if (!f) return
  err.value = null
  try {
    jobs[`staging-${r.id}`] = (await api.post<{ job_id: string }>(`/api/backup/runs/${r.id}/staging`, { files: f.files, database: f.database || null, domain: f.domain })).job_id
    stagingMsg[r.id] = `Building ${f.domain}… the live site is not touched.`
  } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) }
}

// ---------------------------------------------------------------- restore
const restoreMsg = reactive<Record<string, string>>({})
async function restore(r: BackupRun, source: string) {
  try {
    const res = await api.post<{ job_id: string; target_database: string }>(`/api/backup/runs/${r.id}/restore`, { source })
    jobs[`restore-${r.id}-${source}`] = res.job_id
    restoreMsg[`${r.id}-${source}`] = `Restoring into ${res.target_database} — your live database is not touched.`
  } catch (e) { err.value = errorMessage(e) }
}
function finished(key: string) { setTimeout(() => { runs.refresh(); plans.refresh(); dbs.refresh() }, 500); void key }

const diskTotal = computed(() => (disk.data.value ?? []).reduce((a, b) => a + b.bytes, 0))
const lastOk = computed(() => runs.data.value?.find((r) => r.status === 'ok'))

// ---------------------------------------------------------------- design board 08 view data
const host = computed(() => sys.data.value?.hostname ?? 'server')
const rootDisk = computed(() => sys.data.value?.disks?.find((d) => d.mount === '/') ?? sys.data.value?.disks?.[0])
const gb = (b: number) => `${(b / 1024 ** 3).toFixed(1)} GB`
const parts = computed(() => {
  const d = rootDisk.value
  const items = disk.data.value ?? []
  if (!d) return []
  const pick = (label: string) => items.filter((i) => i.label === label).reduce((a, b) => a + b.bytes, 0)
  const groups = [
    { name: 'Project files', bytes: pick('Websites'), c: '#6CB4FF' },
    { name: 'Local backups', bytes: pick('Local backups'), c: '#B79CFF' },
    { name: 'System & packages', bytes: pick('System & packages') + pick('Package cache'), c: '#7B8798' },
    { name: 'Logs', bytes: pick('Logs'), c: '#F7C55E' },
    { name: 'Databases', bytes: pick('MySQL data') + pick('PostgreSQL data'), c: '#3DDC97' },
  ]
  const known = groups.reduce((a, g) => a + g.bytes, 0)
  groups.push({ name: 'Other', bytes: Math.max(0, d.used - known), c: '#C5CEDA' })
  return groups.map((g) => ({ ...g, size: gb(g.bytes), w: `${((g.bytes / d.total) * 100).toFixed(2)}%` }))
})
const diskLabel = computed(() => (rootDisk.value ? `${Math.round(rootDisk.value.used / 1024 ** 3)} of ${Math.round(rootDisk.value.total / 1024 ** 3)} GB used` : 'measuring…'))
const freeGb = computed(() => (rootDisk.value ? gb(rootDisk.value.total - rootDisk.value.used) : '—'))
const cleanup = ref<string | null>(null)
const CLEANUPS = computed(() => [
  { id: 'journal', label: `Shrink system logs · ${gb((disk.data.value ?? []).find((i) => i.label === 'Logs')?.bytes ?? 0)}` },
  { id: 'apt-cache', label: `Clear package cache · ${gb((disk.data.value ?? []).find((i) => i.label === 'Package cache')?.bytes ?? 0)}` },
  { id: 'dev-caches', label: 'Clear Composer/npm caches' },
  { id: 'old-kernels', label: 'Remove unused packages & old kernels' },
])
function runFor(planId: string) { return (runs.data.value ?? []).find((r) => r.plan_id === planId) ?? null }
const jobRows = computed(() => (plans.data.value ?? []).map((p) => {
  const r = runFor(p.id)
  const d = dests.data.value?.find((x) => x.id === p.destination_id)
  return {
    p,
    what: `${p.databases.includes('all') ? 'All databases' : p.databases.map((x) => x.split('/')[1]).join(', ') || 'No databases'}${p.paths.length ? ` + ${p.paths.length} folder${p.paths.length > 1 ? 's' : ''}` : ''}`,
    when: scheduleText(p.schedule).replace(' UTC', ''),
    dest: d ? `Local · ${d.name}` : 'Local only',
    encrypted: !!d?.config?.encrypted,
    keep: `${p.keep_daily} daily, ${p.keep_weekly} weekly, ${p.keep_monthly} monthly`,
    last: !p.enabled ? 'Paused' : !r ? 'Not run yet' : r.status === 'ok' ? `OK · ${formatBytes(r.size_bytes)}` : r.status === 'running' ? 'Running' : 'Failed',
    cls: !p.enabled ? 'off' : !r ? 'warn' : r.status === 'ok' ? 'ok' : r.status === 'running' ? 'info' : 'bad',
  }
}))
async function backupEverything() {
  for (const p of (plans.data.value ?? []).filter((x) => x.enabled)) await runPlan(p)
  if (!(plans.data.value ?? []).length) edit()
}
// restore points
const pointFilter = ref('all')
const pointFilters = computed(() => {
  const srcs = new Set<string>()
  for (const r of runs.data.value ?? []) for (const i of r.items) if (i.kind === 'database') srcs.add(i.source)
  return [{ value: 'all', label: 'All restore points' }, ...[...srcs].sort().map((s) => ({ value: s, label: s.split('/')[1] ?? s, hint: s.split('/')[0] })), { value: 'files', label: 'Project files' }]
})
const points = computed(() => (runs.data.value ?? []).filter((r) => r.status === 'ok')
  .filter((r) => pointFilter.value === 'all' || (pointFilter.value === 'files' ? r.items.some((i) => i.kind === 'files') : r.items.some((i) => i.source === pointFilter.value))))
const pickedRun = ref<string | null>(null)
const picked = computed(() => points.value.find((r) => r.id === pickedRun.value) ?? points.value[0] ?? null)
const restoreSource = ref('')
watch(picked, (r) => { restoreSource.value = r?.items.find((i) => i.kind === 'database' && (pointFilter.value === 'all' || pointFilter.value === 'files' || i.source === pointFilter.value))?.source ?? '' }, { immediate: true })
const restoreInto = ref<'copy' | 'staging'>('copy')
const confirmText = ref('')
const restoreDbName = computed(() => restoreSource.value.split('/')[1] ?? '')
function pointWhere(r: BackupRun) { return r.remote_path ? (r.local_path ? 'Local + off-site' : 'Off-site') : 'Local' }
function pointKind(r: BackupRun) { return planName(r.plan_id) }
function downloadPath(r: BackupRun) {
  const it = r.items.find((i) => pointFilter.value === 'all' || pointFilter.value === 'files' ? true : i.source === pointFilter.value) ?? r.items[0]
  return it && r.local_path ? `runs/${r.plan_id}/${r.stamp}/${it.file}` : null
}
async function startRestore() {
  const r = picked.value
  if (!r) return
  if (restoreInto.value === 'staging') { openStaging(r); return }
  if (restoreSource.value) await restore(r, restoreSource.value)
}
// destinations view
const destCards = computed(() => [
  { key: 'local', name: 'This server', state: 'Active', cls: 'ok', note: `/var/backups/nexacloud · ${gb((disk.data.value ?? []).find((i) => i.label === 'Local backups')?.bytes ?? 0)}`, btn: null as string | null, dest: null as BackupDestination | null },
  ...(dests.data.value ?? []).map((d) => ({ key: d.id, name: d.name, state: d.config.encrypted ? 'Encrypted' : 'Connected', cls: 'ok', note: d.kind === 'sftp' ? `${d.config.user}@${d.config.host}:${d.config.path}` : d.kind === 's3' ? `${d.config.provider ?? 'S3'} · ${d.config.bucket}/${d.config.path}` : String(d.config.path), btn: 'Test connection', dest: d })),
  ...(dests.data.value ?? []).some((d) => d.kind === 'sftp') ? [] : [{ key: 'add-sftp', name: 'Synology / SFTP', state: 'Not set up', cls: 'off', note: 'Your NAS or any server with SSH access', btn: 'Connect', dest: null }],
  ...(dests.data.value ?? []).some((d) => d.kind === 's3') ? [] : [{ key: 'add-s3', name: 'S3 · B2 · Synology C2', state: 'Not set up', cls: 'off', note: 'S3-compatible object storage, low cost', btn: 'Connect', dest: null }],
])
function destButton(c: { key: string; dest: BackupDestination | null }) {
  if (c.dest) return testDest(c.dest)
  dKind.value = c.key === 'add-s3' ? 's3' : 'sftp'
  addingDest.value = true
  setTimeout(() => document.getElementById('dest-form')?.scrollIntoView({ behavior: 'smooth', block: 'start' }), 50)
}
const anyEncrypted = computed(() => (dests.data.value ?? []).some((d) => d.config.encrypted))
</script>

<template>
  <header style="display: flex; flex-wrap: wrap; align-items: flex-end; justify-content: space-between; gap: 16px">
    <div style="display: flex; flex-direction: column; gap: 6px">
      <p class="lbl">{{ host }} / Backups &amp; Storage</p>
      <h1 style="font-size: 30px; font-weight: 600">Backups &amp; Storage</h1>
    </div>
    <div style="display: flex; flex-wrap: wrap; gap: 10px"><button type="button" class="btn" @click="edit()">New backup job</button><button type="button" class="btn btn-p" @click="backupEverything">Back up everything now</button></div>
  </header>
  <p v-if="err" class="small" style="color: var(--bad)" role="alert">{{ err }}</p>

  <section class="card" aria-labelledby="disk" style="padding: 20px; display: flex; flex-direction: column; gap: 16px">
    <div style="display: flex; flex-wrap: wrap; align-items: baseline; justify-content: space-between; gap: 10px">
      <h2 id="disk" style="font-size: 17px; font-weight: 600">Disk · {{ diskLabel }}</h2>
      <span style="font-size: 13px; color: #A3AEBD">{{ freeGb }} free · measured with du</span>
    </div>
    <div style="display: flex; height: 18px; border-radius: 6px; overflow: hidden; background: #1F2731; gap: 2px" role="img" :aria-label="parts.map((p) => `${p.name} ${p.size}`).join(', ') + `, free ${freeGb}`">
      <div v-for="p in parts" :key="p.name" :style="`width: ${p.w}; background: ${p.c}`" />
    </div>
    <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 10px">
      <div v-for="p in parts" :key="p.name" style="display: flex; align-items: center; gap: 10px; padding: 10px 12px; background: #0F1318; border-radius: 10px">
        <span :style="`width: 10px; height: 10px; border-radius: 3px; background: ${p.c}; flex: none`" aria-hidden="true" />
        <span style="flex: 1; font-size: 13px">{{ p.name }}</span>
        <span class="mono" style="font-size: 13px; font-weight: 500">{{ p.size }}</span>
      </div>
      <p v-if="!parts.length" style="font-size: 13px; color: #A3AEBD">Measuring disk usage…</p>
    </div>
    <div style="display: flex; flex-wrap: wrap; gap: 10px; padding-top: 6px; border-top: 1px solid #1B222B">
      <span style="font-size: 13px; font-weight: 600; align-self: center; margin-right: 6px">Free up space</span>
      <button v-for="c in CLEANUPS" :key="c.id" type="button" class="btn btn-s" :aria-pressed="cleanup === c.id" @click="cleanup = cleanup === c.id ? null : c.id">{{ c.label }}</button>
    </div>
    <PlanAction v-if="cleanup" :key="cleanup" :request="{ kind: 'disk_cleanup', params: { action: cleanup } }" label="Preview" apply-label="Clean up" success-text="Done — space freed." @done="(s) => { if (s === 'applied') { cleanup = null; disk.refresh(); sys.refresh() } }" />
  </section>

  <!-- plan editor -->
  <section v-if="editing" class="card card-pad stack" style="gap: 14px" aria-label="Backup plan">
    <div class="row" style="justify-content: space-between"><h2>{{ editing === 'new' ? 'New backup job' : 'Edit backup job' }}</h2><button type="button" class="btn btn-s btn-ghost" @click="editing = null">Close</button></div>
    <div class="fgrid">
      <label class="field"><span>Name</span><input v-model="form.name" class="in" type="text" placeholder="Nightly"></label>
      <label class="field"><span>Copy to</span>
        <SearchSelect v-model="form.destination_id" :options="[{ value: '', label: 'This server only (not recommended)' }, ...(dests.data.value ?? []).map((d) => ({ value: d.id, label: d.name, hint: d.kind }))]" aria-label="Copy to" />
      </label>
      <label class="field"><span>When</span>
        <div class="row" style="gap: 6px">
          <SearchSelect v-model="form.freq" :options="[{ value: 'daily', label: 'Daily' }, { value: 'weekly', label: 'Weekly' }, { value: 'hourly', label: 'Hourly' }, { value: 'manual', label: 'Manual only' }]" aria-label="Frequency" style="width: 150px; min-width: 150px" />
          <SearchSelect v-if="form.freq === 'weekly'" v-model="form.day" :options="['mon','tue','wed','thu','fri','sat','sun']" aria-label="Day" style="width: 100px; min-width: 100px" />
          <input v-if="form.freq === 'daily' || form.freq === 'weekly'" v-model="form.time" class="in" type="time" placeholder="03:00" aria-label="Time (UTC)" style="width: 120px">
        </div>
        <span class="small muted">Times are UTC.</span>
      </label>
      <div class="field"><span>Keep</span>
        <div class="row" style="gap: 6px">
          <label class="small">daily <input v-model.number="form.keep_daily" class="in num" type="number" min="0" max="90" placeholder="7"></label>
          <label class="small">weekly <input v-model.number="form.keep_weekly" class="in num" type="number" min="0" max="52" placeholder="4"></label>
          <label class="small">monthly <input v-model.number="form.keep_monthly" class="in num" type="number" min="0" max="36" placeholder="3"></label>
        </div>
      </div>
    </div>
    <div class="stack" style="gap: 8px">
      <label class="small"><input v-model="form.allDbs" type="checkbox"> All databases (new ones are included automatically)</label>
      <div v-if="!form.allDbs" class="chk-grid">
        <label v-for="d in dbChoices" :key="d" class="small mono"><input v-model="form.dbs" type="checkbox" :value="d"> {{ d }}</label>
      </div>
    </div>
    <label class="field"><span>Folders (one per line, optional) — e.g. uploads and .env</span>
      <textarea v-model="form.paths" class="in mono" rows="3" placeholder="/var/www/shop/storage/app&#10;/var/www/shop/.env" style="padding: 10px; min-height: 80px" />
    </label>
    <label class="small"><input v-model="form.enabled" type="checkbox"> Enabled</label>
    <div><button type="button" class="btn btn-p" @click="savePlan">Save backup job</button>
      <ConfirmAction v-if="editing !== 'new'" label="Delete this job" :question="`Delete ${form.name}? Existing backups stay.`" confirm-label="Delete" @confirm="() => { const p = plans.data.value?.find((x) => x.id === editing); if (p) { delPlan(p); editing = null } }" /></div>
  </section>


  <section class="card" aria-labelledby="jobs" style="overflow: hidden">
    <div style="padding: 16px 20px"><h2 id="jobs" style="font-size: 17px; font-weight: 600">Backup jobs</h2></div>
    <div style="overflow-x: auto">
      <table>
        <thead><tr><th>Job</th><th>Includes</th><th>Schedule</th><th>Goes to</th><th>Keeps</th><th>Last run</th><th><span style="position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0)">Actions</span></th></tr></thead>
        <tbody>
          <template v-for="j in jobRows" :key="j.p.id">
            <tr>
              <td style="font-weight: 600">{{ j.p.name }}</td>
              <td style="font-size: 13px; color: #C5CEDA; white-space: normal">{{ j.what }}</td>
              <td style="font-size: 13px">{{ j.when }}</td>
              <td><div style="display: flex; gap: 6px; align-items: center; font-size: 13px">{{ j.dest }}<span v-if="j.encrypted" class="pill ok">encrypted</span></div></td>
              <td style="font-size: 13px; color: #C5CEDA">{{ j.keep }}</td>
              <td><span :class="`pill ${j.cls}`">{{ j.last }}</span></td>
              <td><div style="display: flex; gap: 6px"><button type="button" class="btn btn-s" @click="runPlan(j.p)">Run now</button><button type="button" class="btn btn-s" @click="edit(j.p)">Edit</button></div></td>
            </tr>
            <tr v-if="jobs[`run-${j.p.id}`]"><td colspan="7" style="white-space: normal"><JobLog :job-id="jobs[`run-${j.p.id}`]!" status="running" @finished="finished(`run-${j.p.id}`)" /></td></tr>
          </template>
          <tr v-if="!jobRows.length"><td colspan="7" style="color: #A3AEBD; white-space: normal">No backup jobs yet. <button type="button" class="btn btn-s btn-p" style="margin-left: 8px" @click="edit()">Create the first one</button></td></tr>
        </tbody>
      </table>
    </div>
  </section>

  <div style="display: flex; flex-wrap: wrap; gap: 16px; align-items: flex-start">
    <section class="card" aria-labelledby="rp" style="flex: 3 1 520px; min-width: 0; padding: 20px; display: flex; flex-direction: column; gap: 12px">
      <div style="display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px">
        <h2 id="rp" style="font-size: 17px; font-weight: 600">Restore points</h2>
        <SearchSelect v-model="pointFilter" :options="pointFilters" aria-label="Filter restore points" style="width: 220px" />
      </div>
      <div v-for="r in points.slice(0, 12)" :key="r.id" :style="`display: flex; flex-wrap: wrap; align-items: center; gap: 12px; padding: 12px 14px; background: ${picked?.id === r.id ? '#17140D' : '#0F1318'}; border: 1px solid ${picked?.id === r.id ? '#4A3B1C' : '#222A34'}; border-radius: 10px`">
        <span style="flex: 1 1 200px; display: flex; flex-direction: column; gap: 2px"><span style="font-size: 14px; font-weight: 600">{{ formatDateTime(r.started_at) }}</span><span style="font-size: 12px; color: #A3AEBD">{{ pointKind(r) }} · {{ formatBytes(r.size_bytes) }} · {{ pointWhere(r) }}</span></span>
        <a v-if="downloadPath(r)" class="btn btn-s" :href="`/api/backups/download?file=${encodeURIComponent(downloadPath(r)!)}`">Download</a>
        <button type="button" class="btn btn-s" @click="pickedRun = r.id">Restore…</button>
      </div>
      <p v-if="!points.length" style="font-size: 13px; color: #A3AEBD">No restore points yet — run a backup job.</p>
    </section>

    <section v-if="picked" class="card" aria-labelledby="rs" style="flex: 2 1 360px; min-width: 0; padding: 20px; display: flex; flex-direction: column; gap: 14px; border-color: #4A3B1C">
      <h2 id="rs" style="font-size: 17px; font-weight: 600">Restore {{ restoreInto === 'staging' ? 'site to staging' : (restoreDbName || 'files') }}</h2>
      <p style="font-size: 13px; color: #C5CEDA">From <strong>{{ formatDateTime(picked.started_at) }} ({{ pointKind(picked).toLowerCase() }})</strong></p>
      <label v-if="restoreInto === 'copy'" style="display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Database</span>
        <SearchSelect v-model="restoreSource" :options="picked.items.filter((i) => i.kind === 'database').map((i) => ({ value: i.source, label: i.source.split('/')[1] ?? i.source, hint: i.source.split('/')[0] }))" placeholder="No database in this backup" aria-label="Database to restore" />
      </label>
      <fieldset style="border: 0; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 8px">
        <legend style="font-size: 13px; font-weight: 500; color: #C5CEDA; margin-bottom: 8px">Restore into</legend>
        <label :style="`display: flex; gap: 10px; padding: 12px; border: 1px solid ${restoreInto === 'copy' ? '#3DDC97' : '#2A3441'}; ${restoreInto === 'copy' ? 'background: #13211B;' : ''} border-radius: 10px; cursor: pointer`"><input v-model="restoreInto" type="radio" name="into" value="copy" style="accent-color: #3DDC97; margin-top: 2px"><span style="display: flex; flex-direction: column; gap: 2px"><span style="font-size: 14px; font-weight: 600">A new copy: {{ restoreDbName || 'database' }}_restore_{{ picked.stamp.slice(0, 8) }}</span><span style="font-size: 12px; color: #A3AEBD">Safe. Inspect it first, live site untouched.</span></span></label>
        <label :style="`display: flex; gap: 10px; padding: 12px; border: 1px solid ${restoreInto === 'staging' ? '#3DDC97' : '#2A3441'}; ${restoreInto === 'staging' ? 'background: #13211B;' : ''} border-radius: 10px; cursor: ${picked.items.some((i) => i.kind === 'files') ? 'pointer' : 'not-allowed'}; ${picked.items.some((i) => i.kind === 'files') ? '' : 'opacity: .5'}`"><input v-model="restoreInto" type="radio" name="into" value="staging" :disabled="!picked.items.some((i) => i.kind === 'files')" style="accent-color: #3DDC97; margin-top: 2px"><span style="display: flex; flex-direction: column; gap: 2px"><span style="font-size: 14px; font-weight: 600">A full staging copy of the site</span><span style="font-size: 12px; color: #A3AEBD">Files + database + .env on a new domain with its own user. {{ picked.items.some((i) => i.kind === 'files') ? '' : 'This backup has no folders.' }}</span></span></label>
        <label style="display: flex; gap: 10px; padding: 12px; border: 1px solid #2A3441; border-radius: 10px; opacity: .5; cursor: not-allowed"><input type="radio" name="into" disabled style="margin-top: 2px"><span style="display: flex; flex-direction: column; gap: 2px"><span style="font-size: 14px; font-weight: 600">Replace {{ restoreDbName || 'the live database' }}</span><span style="font-size: 12px; color: #A3AEBD">Coming soon — restore into a copy, check it, then point the app at it.</span></span></label>
      </fieldset>
      <label v-if="restoreInto === 'copy'" style="display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Type {{ restoreDbName }} to confirm</span><input v-model="confirmText" class="in mono" type="text" :placeholder="restoreDbName"></label>
      <template v-if="restoreInto === 'staging' && staging[picked.id]">
        <div class="fgrid">
          <label class="field"><span>Folder</span><SearchSelect v-model="staging[picked.id]!.files" :options="picked.items.filter((x) => x.kind === 'files').map((i) => i.source)" mono aria-label="Folder" /></label>
          <label class="field"><span>Database</span><SearchSelect v-model="staging[picked.id]!.database" :options="[{ value: '', label: '(none)' }, ...picked.items.filter((x) => x.kind === 'database').map((i) => i.source)]" mono aria-label="Database" /></label>
          <label class="field"><span>Staging domain</span><input v-model="staging[picked.id]!.domain" class="in mono" type="text" placeholder="staging.example.com"></label>
        </div>
        <button type="button" class="btn btn-p" @click="startStaging(picked)">Restore to staging</button>
        <span v-if="stagingMsg[picked.id]" class="small" style="color: var(--ok)">{{ stagingMsg[picked.id] }}</span>
        <JobLog v-if="jobs[`staging-${picked.id}`]" :job-id="jobs[`staging-${picked.id}`]!" status="running" @finished="finished('restore')" />
      </template>
      <template v-else>
        <button type="button" class="btn btn-p" :disabled="restoreInto === 'copy' ? (!restoreSource || confirmText !== restoreDbName) : false" @click="startRestore">{{ restoreInto === 'staging' ? 'Set up staging copy' : 'Start restore' }}</button>
        <span v-if="restoreMsg[`${picked.id}-${restoreSource}`]" class="small" style="color: var(--ok)">{{ restoreMsg[`${picked.id}-${restoreSource}`] }}</span>
        <JobLog v-if="jobs[`restore-${picked.id}-${restoreSource}`]" :job-id="jobs[`restore-${picked.id}-${restoreSource}`]!" status="running" @finished="finished('restore')" />
      </template>
    </section>
  </div>

  <section aria-labelledby="dest" style="display: flex; flex-direction: column; gap: 12px">
    <h2 id="dest" style="font-size: 18px; font-weight: 600">Destinations</h2>
    <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 12px">
      <div v-for="c in destCards" :key="c.key" class="card" style="padding: 16px; display: flex; flex-direction: column; gap: 8px">
        <div style="display: flex; align-items: center; justify-content: space-between; gap: 8px"><span style="font-size: 15px; font-weight: 600">{{ c.name }}</span><span :class="`pill ${c.cls}`">{{ c.state }}</span></div>
        <span class="mono" style="font-size: 12px; color: #A3AEBD; overflow-wrap: anywhere">{{ c.note }}</span>
        <div v-if="c.btn" style="display: flex; gap: 6px">
          <button type="button" class="btn btn-s" style="flex: 1" @click="destButton(c)">{{ c.btn }}</button>
          <ConfirmAction v-if="c.dest" label="Remove" :question="`Remove ${c.name}?`" confirm-label="Remove" @confirm="delDest(c.dest!)" />
        </div>
        <JobLog v-if="c.dest && jobs[`test-${c.dest.id}`]" :job-id="jobs[`test-${c.dest.id}`]!" status="running" />
      </div>
    </div>
    <div v-if="addingDest" id="dest-form" class="card add stack" style="gap: 14px">
      <div class="seg" role="group" aria-label="Destination type">
        <button type="button" :class="{ on: dKind === 'sftp' }" @click="dKind = 'sftp'">Synology / SSH (SFTP)</button>
        <button type="button" :class="{ on: dKind === 's3' }" @click="dKind = 's3'">S3-compatible</button>
        <button type="button" :class="{ on: dKind === 'local' }" @click="dKind = 'local'">Folder on this server</button>
      </div>
      <label class="field"><span>Name</span><input v-model="dName" class="in" type="text" placeholder="Synology NAS"></label>

      <template v-if="dKind === 'sftp'">
        <ol class="steps small text-2">
          <li>On the Synology: <strong>Control Panel → File Services → FTP → Enable SFTP</strong>, and <strong>User → Advanced → Enable user home service</strong>. Create a user (e.g. <span class="mono">backup</span>) with access to a shared folder.</li>
          <li>Make the NAS reachable from this server — easiest: install <strong>Tailscale</strong> on both (Synology Package Center → Tailscale) and use the NAS's <span class="mono">100.x.x.x</span> address. Or use DDNS + a forwarded port.</li>
          <li>Add this key to the NAS user's <span class="mono">~/.ssh/authorized_keys</span> (DSM: File Station → homes/backup/.ssh):</li>
        </ol>
        <div class="key"><code class="mono">{{ pubKey ?? 'loading…' }}</code><button type="button" class="btn btn-s" @click="copyKey">Copy</button></div>
        <div class="fgrid">
          <label class="field"><span>Host (Tailscale IP or DDNS name)</span><input v-model="sftp.host" class="in mono" type="text" placeholder="100.101.102.103"></label>
          <label class="field"><span>Port</span><input v-model.number="sftp.port" class="in mono" type="number" placeholder="22"></label>
          <label class="field"><span>User</span><input v-model="sftp.user" class="in mono" type="text" placeholder="backup"></label>
          <label class="field"><span>Folder on the NAS</span><input v-model="sftp.path" class="in mono" type="text" placeholder="/volume1/backups/vps"></label>
        </div>
      </template>

      <template v-if="dKind === 's3'">
        <div class="row wrap" style="gap: 6px"><button v-for="p in S3_PRESETS" :key="p.id" type="button" class="btn btn-s" :class="{ 'btn-p': s3.provider === p.id }" @click="preset(p.id)">{{ p.label }}</button></div>
        <div class="fgrid">
          <label class="field"><span>Endpoint</span><input v-model="s3.endpoint" class="in mono" type="text" placeholder="(empty for AWS)"></label>
          <label class="field"><span>Region</span><input v-model="s3.region" class="in mono" type="text" placeholder="eu-central-1"></label>
          <label class="field"><span>Bucket</span><input v-model="s3.bucket" class="in mono" type="text" placeholder="my-backups"></label>
          <label class="field"><span>Folder in bucket</span><input v-model="s3.path" class="in mono" type="text" placeholder="vps"></label>
          <label class="field"><span>Access key</span><input v-model="s3.access_key" class="in mono" type="text" autocomplete="off" placeholder="Access key ID"></label>
          <label class="field"><span>Secret key</span><input v-model="s3.secret_key" class="in mono" type="password" autocomplete="new-password" placeholder="Secret access key"></label>
        </div>
      </template>

      <template v-if="dKind === 'local'">
        <label class="field"><span>Folder (e.g. a mounted second disk)</span><input v-model="local.path" class="in mono" type="text" placeholder="/mnt/backup-disk/nexacloud"></label>
      </template>

      <fieldset v-if="dKind !== 'local'" class="enc">
        <legend><label class="row" style="gap: 8px"><input v-model="encrypt" type="checkbox"> <strong>Encrypt backups before they leave this server</strong></label></legend>
        <template v-if="encrypt">
          <p class="small text-2">Files and file names are encrypted with this password (rclone crypt). The NAS or cloud provider only ever sees scrambled data. <strong>Save this password somewhere safe</strong> (a password manager): without it, the off-site copies cannot be restored on another server.</p>
          <div class="row wrap" style="gap: 8px">
            <input v-model="encPass" class="in mono" type="text" autocomplete="off" placeholder="at least 12 characters" style="flex: 1; min-width: 220px" aria-label="Encryption password">
            <button type="button" class="btn btn-s" @click="genPass">Generate</button>
          </div>
        </template>
      </fieldset>
      <div class="row" style="gap: 8px"><button type="button" class="btn btn-p" @click="addDest">Save destination</button><span class="small muted">Then click “Test connection”.</span></div>
    </div>
  
    <p style="font-size: 12px; color: #A3AEBD">{{ anyEncrypted ? 'Off-site copies to destinations marked “Encrypted” are encrypted on this server (rclone crypt) before upload. Keep the encryption password outside this server.' : 'Tip: turn on encryption when connecting a destination — backups are then encrypted on this server before they leave it.' }}</p>
  </section>
</template>

<style scoped>
.stats { grid-template-columns: repeat(auto-fit, minmax(190px, 1fr)); gap: 12px; display: grid; }
.mini { padding: 16px; display: flex; flex-direction: column; gap: 6px; }
.mini-num { font-family: var(--font-head); font-size: 22px; font-weight: 600; }
.fgrid { display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 12px; }
.field { display: flex; flex-direction: column; gap: 6px; }
.field > span:first-child { font-size: 13px; font-weight: 500; color: var(--text-2); }
.in { min-height: 40px; padding: 0 10px; background: var(--sunken, #0F1318); border: 1px solid var(--btn-line, #2A3441); border-radius: 8px; color: var(--text); font-size: 14px; width: 100%; box-sizing: border-box; }
.num { width: 64px; margin-left: 4px; }
.chk-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); gap: 6px; }
input[type=checkbox] { accent-color: var(--accent); width: 16px; height: 16px; vertical-align: middle; }
.dest-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 12px; }
.dest { padding: 14px; display: flex; flex-direction: column; gap: 8px; background: var(--sunken, #0F1318); }
.add { padding: 16px; background: var(--sunken, #0F1318); }
.seg { display: flex; flex-wrap: wrap; gap: 4px; padding: 4px; background: var(--card, #12171D); border: 1px solid var(--line, #222A34); border-radius: 10px; align-self: flex-start; }
.seg button { min-height: 36px; padding: 0 14px; border-radius: 7px; border: 0; background: transparent; color: var(--text-2); font-weight: 600; cursor: pointer; }
.seg button.on { background: #1F2833; color: var(--text); }
.steps { margin: 0; padding-left: 18px; display: flex; flex-direction: column; gap: 6px; line-height: 1.5; }
.key { display: flex; gap: 8px; align-items: center; background: #07090C; border: 1px solid #1B222B; border-radius: 10px; padding: 10px 12px; }
.key code { flex: 1; font-size: 12px; overflow-wrap: anywhere; color: var(--text-2); }
.disk-row { display: flex; align-items: center; gap: 12px; }
.bar { flex: 1; height: 8px; border-radius: 99px; background: #1F2731; overflow: hidden; }
.bar span { display: block; height: 100%; background: var(--accent); }
.enc { border: 1px solid var(--card-line); border-radius: 10px; padding: 10px 14px 14px; margin: 0; display: flex; flex-direction: column; gap: 10px; }
.stg { padding: 12px; background: #0F1318; border: 1px solid #222A34; border-radius: 10px; }
</style>
