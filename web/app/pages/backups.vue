<script setup lang="ts">
import type { BackupDestination, BackupPlan, BackupRun, DatabasesInfo, DiskUsageItem } from '~/types/api'

useHead({ title: 'Backups · NexaCloud' })

const api = useApi()
const dests = useResource<BackupDestination[]>('/api/backup/destinations')
const plans = useResource<BackupPlan[]>('/api/backup/plans')
const runs = useResource<BackupRun[]>('/api/backup/runs?limit=30', { interval: 15_000 })
const dbs = useResource<DatabasesInfo>('/api/databases')
const disk = useResource<DiskUsageItem[]>('/api/disk')

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
</script>

<template>
  <PageHeader title="Backups" crumb="Databases and folders, on a schedule, copied off the server">
    <button type="button" class="btn btn-p" @click="edit()"><AppIcon name="plus" :size="16" :stroke="2.2" /> New backup plan</button>
  </PageHeader>

  <p v-if="err" class="small" style="color: var(--bad)" role="alert">{{ err }}</p>

  <section class="grid-cards stats" aria-label="Backup status">
    <div class="card mini"><span class="lbl">Last good backup</span><span class="mini-num">{{ lastOk ? formatRelative(lastOk.finished_at ?? lastOk.started_at) : 'Never' }}</span></div>
    <div class="card mini"><span class="lbl">Plans</span><span class="mini-num">{{ plans.data.value?.length ?? 0 }}</span></div>
    <div class="card mini"><span class="lbl">Destinations</span><span class="mini-num">{{ dests.data.value?.length ?? 0 }}</span></div>
    <div class="card mini"><span class="lbl">Retention</span><span class="mini-num" style="font-size: 16px">7 daily · 4 weekly · 3 monthly</span></div>
  </section>

  <!-- plan editor -->
  <section v-if="editing" class="card card-pad stack" style="gap: 14px" aria-label="Backup plan">
    <div class="row" style="justify-content: space-between"><h2>{{ editing === 'new' ? 'New backup plan' : 'Edit backup plan' }}</h2><button type="button" class="btn btn-s btn-ghost" @click="editing = null">Close</button></div>
    <div class="fgrid">
      <label class="field"><span>Name</span><input v-model="form.name" class="in" type="text"></label>
      <label class="field"><span>Copy to</span>
        <select v-model="form.destination_id" class="in">
          <option value="">This server only (not recommended)</option>
          <option v-for="d in dests.data.value ?? []" :key="d.id" :value="d.id">{{ d.name }} ({{ d.kind }})</option>
        </select>
      </label>
      <label class="field"><span>When</span>
        <div class="row" style="gap: 6px">
          <select v-model="form.freq" class="in"><option value="daily">Daily</option><option value="weekly">Weekly</option><option value="hourly">Hourly</option><option value="manual">Manual only</option></select>
          <select v-if="form.freq === 'weekly'" v-model="form.day" class="in" style="width: 90px"><option v-for="d in ['mon','tue','wed','thu','fri','sat','sun']" :key="d">{{ d }}</option></select>
          <input v-if="form.freq === 'daily' || form.freq === 'weekly'" v-model="form.time" class="in" type="time" style="width: 120px">
        </div>
        <span class="small muted">Times are UTC.</span>
      </label>
      <div class="field"><span>Keep</span>
        <div class="row" style="gap: 6px">
          <label class="small">daily <input v-model.number="form.keep_daily" class="in num" type="number" min="0" max="90"></label>
          <label class="small">weekly <input v-model.number="form.keep_weekly" class="in num" type="number" min="0" max="52"></label>
          <label class="small">monthly <input v-model.number="form.keep_monthly" class="in num" type="number" min="0" max="36"></label>
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
    <div><button type="button" class="btn btn-p" @click="savePlan">Save plan</button></div>
  </section>

  <!-- plans -->
  <section class="card" aria-labelledby="plans-h" style="overflow: hidden">
    <div class="card-head"><h2 id="plans-h">Backup plans</h2></div>
    <div v-if="!plans.data.value?.length" class="empty">
      <strong>No backup plans yet</strong>
      <span>Create one: every night, all databases, copied to your Synology or S3.</span>
      <button type="button" class="btn btn-s btn-p" @click="edit()">New backup plan</button>
    </div>
    <div v-else class="table-wrap"><table>
      <thead><tr><th>Plan</th><th>What</th><th>When</th><th>Copy to</th><th>Keeps</th><th>Last run</th><th /></tr></thead>
      <tbody>
        <template v-for="p in plans.data.value" :key="p.id">
          <tr>
            <td><strong>{{ p.name }}</strong><StatusPill v-if="!p.enabled" tone="off" label="Paused" style="margin-left: 8px" /></td>
            <td class="small">{{ p.databases.includes('all') ? 'All databases' : `${p.databases.length} database(s)` }}<template v-if="p.paths.length"> + {{ p.paths.length }} folder(s)</template></td>
            <td class="small">{{ scheduleText(p.schedule) }}</td>
            <td class="small">{{ destName(p.destination_id) }}</td>
            <td class="small">{{ p.keep_daily }}d · {{ p.keep_weekly }}w · {{ p.keep_monthly }}m</td>
            <td class="small">{{ p.last_run_at ? formatRelative(p.last_run_at) : 'Never' }}</td>
            <td><div class="row" style="gap: 6px; justify-content: flex-end">
              <button type="button" class="btn btn-s btn-p" @click="runPlan(p)"><AppIcon name="play" :size="12" :stroke="2" /> Run now</button>
              <button type="button" class="btn btn-s" @click="edit(p)">Edit</button>
              <ConfirmAction label="Delete" :question="`Delete plan ${p.name}?`" confirm-label="Delete" @confirm="delPlan(p)" />
            </div></td>
          </tr>
          <tr v-if="jobs[`run-${p.id}`]"><td colspan="7"><JobLog :job-id="jobs[`run-${p.id}`]!" status="running" @finished="finished(`run-${p.id}`)" /></td></tr>
        </template>
      </tbody>
    </table></div>
  </section>

  <!-- runs -->
  <section class="card" aria-labelledby="runs-h" style="overflow: hidden">
    <div class="card-head"><h2 id="runs-h">Restore points</h2><span class="small muted">Restoring always creates a copy (<span class="mono">name_restore_date</span>) — never overwrites the live database.</span></div>
    <div v-if="!runs.data.value?.length" class="empty"><strong>No backups yet</strong><span>Run a plan to create the first restore point.</span></div>
    <div v-else class="table-wrap"><table>
      <thead><tr><th>When</th><th>Plan</th><th>Status</th><th>Size</th><th>Contents</th><th>Stored</th></tr></thead>
      <tbody>
        <template v-for="r in runs.data.value" :key="r.id">
        <tr>
          <td class="small">{{ formatDateTime(r.started_at) }}<div class="muted">{{ formatRelative(r.started_at) }}</div></td>
          <td class="small">{{ planName(r.plan_id) }}</td>
          <td><StatusPill :tone="r.status === 'ok' ? 'ok' : r.status === 'running' ? 'info' : 'bad'" :label="r.status === 'ok' ? 'OK' : r.status" /><div v-if="r.error" class="small" style="color: var(--bad); white-space: normal; max-width: 260px">{{ r.error }}</div></td>
          <td class="mono small">{{ formatBytes(r.size_bytes) }}</td>
          <td style="white-space: normal">
            <div v-for="it in r.items" :key="it.file" class="row wrap" style="gap: 8px; margin-bottom: 4px">
              <span class="tag mono">{{ it.source }}</span>
              <span class="small muted">{{ formatBytes(it.size_bytes) }}</span>
              <ConfirmAction v-if="it.kind === 'database' && r.status === 'ok'" label="Restore…" :question="`Restore ${it.source} into a new copy?`" confirm-label="Restore copy" @confirm="restore(r, it.source)" />
              <span v-if="restoreMsg[`${r.id}-${it.source}`]" class="small" style="color: var(--ok)">{{ restoreMsg[`${r.id}-${it.source}`] }}</span>
              <JobLog v-if="jobs[`restore-${r.id}-${it.source}`]" :job-id="jobs[`restore-${r.id}-${it.source}`]!" status="running" @finished="finished('restore')" />
            </div>
          </td>
          <td class="small" style="white-space: normal">{{ r.remote_path ? 'Server + off-site' : 'This server' }}
            <div v-if="r.status === 'ok' && r.items.some((i) => i.kind === 'files')" style="margin-top: 6px"><button type="button" class="btn btn-s" @click="openStaging(r)">Restore to staging…</button></div>
          </td>
        </tr>
        <tr v-if="staging[r.id]">
          <td colspan="6" style="white-space: normal">
            <div class="stack stg" style="gap: 10px">
              <strong>Restore a full copy of the site to staging</strong>
              <p class="small text-2">Files go to a new folder, the database into a new copy with its own user, and the copy's .env is pointed at them (debug off, mail to the log, no queue workers). It gets its own Linux user and nginx site. Your live site and database are not touched.</p>
              <div class="fgrid">
                <label class="field"><span>Folder</span><select v-model="staging[r.id]!.files" class="in mono"><option v-for="i in r.items.filter((x) => x.kind === 'files')" :key="i.source" :value="i.source">{{ i.source }}</option></select></label>
                <label class="field"><span>Database</span><select v-model="staging[r.id]!.database" class="in mono"><option value="">(none)</option><option v-for="i in r.items.filter((x) => x.kind === 'database')" :key="i.source" :value="i.source">{{ i.source }}</option></select></label>
                <label class="field"><span>Staging domain</span><input v-model="staging[r.id]!.domain" class="in mono" type="text"></label>
              </div>
              <div class="row wrap" style="gap: 8px"><button type="button" class="btn btn-p btn-s" @click="startStaging(r)">Restore to staging</button><span v-if="stagingMsg[r.id]" class="small" style="color: var(--ok)">{{ stagingMsg[r.id] }}</span></div>
              <JobLog v-if="jobs[`staging-${r.id}`]" :job-id="jobs[`staging-${r.id}`]!" status="running" @finished="finished('restore')" />
            </div>
          </td>
        </tr>
        </template>
      </tbody>
    </table></div>
  </section>

  <!-- destinations -->
  <section class="card card-pad stack" aria-labelledby="dest-h" style="gap: 14px">
    <div class="row" style="justify-content: space-between"><h2 id="dest-h">Destinations</h2><button type="button" class="btn btn-s" @click="addingDest = !addingDest"><AppIcon name="plus" :size="14" :stroke="2" /> Add destination</button></div>
    <div class="dest-grid">
      <article v-for="d in dests.data.value ?? []" :key="d.id" class="card dest">
        <div class="row" style="justify-content: space-between"><strong>{{ d.name }}</strong><span class="row" style="gap: 6px"><StatusPill v-if="d.config.encrypted" tone="ok" label="Encrypted" /><span class="tag">{{ d.kind === 'sftp' ? 'SFTP' : d.kind === 's3' ? 'S3' : 'Folder' }}</span></span></div>
        <span class="small muted mono">{{ d.kind === 'sftp' ? `${d.config.user}@${d.config.host}:${d.config.path}` : d.kind === 's3' ? `${d.config.bucket}/${d.config.path}` : d.config.path }}</span>
        <div class="row" style="gap: 6px">
          <button type="button" class="btn btn-s" @click="testDest(d)">Test connection</button>
          <ConfirmAction label="Remove" :question="`Remove ${d.name}?`" confirm-label="Remove" @confirm="delDest(d)" />
        </div>
        <JobLog v-if="jobs[`test-${d.id}`]" :job-id="jobs[`test-${d.id}`]!" status="running" />
      </article>
      <div v-if="!dests.data.value?.length && !addingDest" class="small muted">No destinations yet — backups stay on this server only. Add your Synology NAS or an S3 bucket so a broken server doesn't take your backups with it.</div>
    </div>

    <div v-if="addingDest" class="card add stack" style="gap: 14px">
      <div class="seg" role="group" aria-label="Destination type">
        <button type="button" :class="{ on: dKind === 'sftp' }" @click="dKind = 'sftp'">Synology / SSH (SFTP)</button>
        <button type="button" :class="{ on: dKind === 's3' }" @click="dKind = 's3'">S3-compatible</button>
        <button type="button" :class="{ on: dKind === 'local' }" @click="dKind = 'local'">Folder on this server</button>
      </div>
      <label class="field"><span>Name</span><input v-model="dName" class="in" type="text"></label>

      <template v-if="dKind === 'sftp'">
        <ol class="steps small text-2">
          <li>On the Synology: <strong>Control Panel → File Services → FTP → Enable SFTP</strong>, and <strong>User → Advanced → Enable user home service</strong>. Create a user (e.g. <span class="mono">backup</span>) with access to a shared folder.</li>
          <li>Make the NAS reachable from this server — easiest: install <strong>Tailscale</strong> on both (Synology Package Center → Tailscale) and use the NAS's <span class="mono">100.x.x.x</span> address. Or use DDNS + a forwarded port.</li>
          <li>Add this key to the NAS user's <span class="mono">~/.ssh/authorized_keys</span> (DSM: File Station → homes/backup/.ssh):</li>
        </ol>
        <div class="key"><code class="mono">{{ pubKey ?? 'loading…' }}</code><button type="button" class="btn btn-s" @click="copyKey">Copy</button></div>
        <div class="fgrid">
          <label class="field"><span>Host (Tailscale IP or DDNS name)</span><input v-model="sftp.host" class="in mono" type="text" placeholder="100.101.102.103"></label>
          <label class="field"><span>Port</span><input v-model.number="sftp.port" class="in mono" type="number"></label>
          <label class="field"><span>User</span><input v-model="sftp.user" class="in mono" type="text"></label>
          <label class="field"><span>Folder on the NAS</span><input v-model="sftp.path" class="in mono" type="text"></label>
        </div>
      </template>

      <template v-if="dKind === 's3'">
        <div class="row wrap" style="gap: 6px"><button v-for="p in S3_PRESETS" :key="p.id" type="button" class="btn btn-s" :class="{ 'btn-p': s3.provider === p.id }" @click="preset(p.id)">{{ p.label }}</button></div>
        <div class="fgrid">
          <label class="field"><span>Endpoint</span><input v-model="s3.endpoint" class="in mono" type="text" placeholder="(empty for AWS)"></label>
          <label class="field"><span>Region</span><input v-model="s3.region" class="in mono" type="text"></label>
          <label class="field"><span>Bucket</span><input v-model="s3.bucket" class="in mono" type="text"></label>
          <label class="field"><span>Folder in bucket</span><input v-model="s3.path" class="in mono" type="text"></label>
          <label class="field"><span>Access key</span><input v-model="s3.access_key" class="in mono" type="text" autocomplete="off"></label>
          <label class="field"><span>Secret key</span><input v-model="s3.secret_key" class="in mono" type="password" autocomplete="new-password"></label>
        </div>
      </template>

      <template v-if="dKind === 'local'">
        <label class="field"><span>Folder (e.g. a mounted second disk)</span><input v-model="local.path" class="in mono" type="text"></label>
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
  </section>

  <!-- disk -->
  <section class="card card-pad stack" aria-labelledby="disk-h" style="gap: 12px">
    <h2 id="disk-h">Disk usage</h2>
    <div v-if="disk.data.value" class="stack" style="gap: 8px">
      <div v-for="d in disk.data.value" :key="d.path" class="disk-row">
        <span class="small" style="width: 160px">{{ d.label }}</span>
        <div class="bar"><span :style="{ width: `${diskTotal ? Math.max(1, (d.bytes / diskTotal) * 100) : 0}%` }" /></div>
        <span class="mono small" style="width: 90px; text-align: right">{{ formatBytes(d.bytes) }}</span>
      </div>
    </div>
    <div class="row wrap" style="gap: 10px; align-items: flex-start">
      <PlanAction :request="{ kind: 'disk_cleanup', params: { action: 'journal' } }" label="Shrink system logs" success-text="Logs trimmed." @done="disk.refresh()" />
      <PlanAction :request="{ kind: 'disk_cleanup', params: { action: 'apt-cache' } }" label="Clear package cache" success-text="Cache cleared." @done="disk.refresh()" />
      <PlanAction :request="{ kind: 'disk_cleanup', params: { action: 'old-kernels' } }" label="Remove unused packages" success-text="Done." @done="disk.refresh()" />
    </div>
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
