<script setup lang="ts">
import type { PhpInfo } from '~/types/api'

const route = useRoute()
const api = useApi()
const id = computed(() => String(route.params.id))
const enc = computed(() => encodeURIComponent(id.value))
useHead({ title: () => `${id.value} · NexaCloud` })

interface Step { name: string; cmd: string; status: string; secs: number | null; result: string }
interface DeployRow { id: string; job_id: string | null; status: string; trigger: string; commit: string | null; message: string | null; author: string | null; previous_commit: string | null; started_at: string; finished_at: string | null; error: string | null; steps: Step[]; release: string | null }
interface ProjectDetail {
  id: string; site_id: string; kind: string; dir: string; repo: string; branch: string; run_as: string; script: string
  migrate: boolean; auto_deploy: boolean; webhook_secret: string; webhook_path: string; deploy_key: string | null; needs_key: boolean
  actions_yaml: string; last_commit: string | null; deploys: DeployRow[]; strategy: string; keep_releases: number; run_tests: boolean; notify: boolean
}
interface Bucket { ts: number; p50: number | null; p95: number | null; checks: number; failures: number }
interface Worker { name: string; queue: string; processes: number; running: number; state: string; uptime: string | null; pending: number | null; failed_24h: number | null }
interface ErrItem { level: string; when: string; count: number; msg: string }
interface Overview {
  site: { id: string; url: string; ssl: boolean; ssl_days: number | null; php_version: string | null; runtime: string | null; managed: boolean }
  facts: { laravel?: string | null; queue?: string | null; db?: { connection: string; database: string } | null; queues?: Record<string, number | null>; schedule?: { command: string; expression: string; description: string | null; next: string }[]; failed_24h?: number | null; error?: string; env?: string | null; debug?: boolean | null; dir_bytes?: number | null; logs_bytes?: number | null; collected_at?: number }
  workers: Worker[]
  scheduler: { enabled: boolean; last_run: number | null }
  maintenance: boolean
  errors: { file: string; items: ErrItem[]; last_hour: number }
  monitor: { checks: number; uptime: number | null; last_code: number | null; last_ms: number | null; last_at: number | null; failures_1h: number; checks_1h: number; buckets: Bucket[]; interval_secs: number }
  app_dir: string
}
interface EnvVar { key: string; value: string | null; secret: boolean }

const p = useResource<ProjectDetail>(`/api/projects/${enc.value}`, { interval: 10_000 })
const ov = useResource<Overview>(`/api/projects/${enc.value}/overview`, { interval: 15_000 })
const php = useResource<PhpInfo>('/api/php')
const status = useResource<{ artisan: string[] }>(`/api/projects/${enc.value}/status`)

type Tab = 'overview' | 'deploys' | 'env' | 'logs' | 'pipeline' | 'setup'
const tab = ref<Tab>(route.query.new ? 'setup' : ((route.query.tab as Tab) || 'overview'))
const err = ref<string | null>(null)
const deployJob = ref<string | null>(null)
const isLaravel = computed(() => p.data.value?.kind === 'laravel')
const tabs: [Tab, string][] = [
  ['overview', 'Overview'], ['deploys', 'Deploys'], ['env', 'Environment'], ['logs', 'Logs'], ['pipeline', 'Pipeline'], ['setup', 'GitHub setup'],
]

function fail(e: unknown) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) }

// ------------------------------------------------------------------ header
const lastDeploy = computed(() => p.data.value?.deploys?.[0] ?? null)
const pendingTotal = computed(() => {
  const q = ov.data.value?.facts?.queues
  if (!q) return null
  const vals = Object.values(q).filter((v): v is number => typeof v === 'number')
  return vals.length ? vals.reduce((a, b) => a + b, 0) : null
})
const phpEol = computed(() => {
  const v = ov.data.value?.site.php_version
  const info = php.data.value?.versions.find((x) => x.version === v)
  return info ? phpSupport(info) : null
})
const health = computed<{ tone: 'ok' | 'warn' | 'bad' | 'info' | 'off'; label: string }>(() => {
  const o = ov.data.value
  if (!o) return { tone: 'off', label: 'Loading' }
  const code = o.monitor.last_code
  if (code !== null && (code === 0 || code >= 500)) return { tone: 'bad', label: code === 0 ? 'Down' : `HTTP ${code}` }
  if (lastDeploy.value && ['failed', 'rolled_back'].includes(lastDeploy.value.status)) return { tone: 'bad', label: 'Last deploy failed' }
  if (o.workers.some((w) => w.state !== 'RUNNING')) return { tone: 'bad', label: 'Worker stopped' }
  if ((pendingTotal.value ?? 0) > 500) return { tone: 'warn', label: 'Queue backlog' }
  if (phpEol.value && phpEol.value.tone === 'bad') return { tone: 'bad', label: 'PHP end-of-life' }
  if (o.facts?.error && isLaravel.value) return { tone: 'info', label: 'Not deployed yet' }
  if (o.maintenance) return { tone: 'warn', label: 'Maintenance mode' }
  if (code === null) return { tone: 'info', label: 'Collecting data' }
  return { tone: 'ok', label: 'Healthy' }
})
const headTags = computed(() => {
  const d = p.data.value
  const o = ov.data.value
  if (!d) return []
  const t: string[] = []
  if (isLaravel.value) t.push(o?.facts?.laravel ? `Laravel ${o.facts.laravel}` : 'Laravel')
  else t.push(d.kind === 'php' ? 'Plain PHP' : d.kind)
  if (o?.site.php_version) t.push(`PHP ${o.site.php_version}`)
  if (o?.site.runtime === 'frankenphp') t.push('FrankenPHP')
  if (o?.facts?.db?.connection) t.push(`${o.facts.db.connection}${o.facts.db.database ? ' · ' + o.facts.db.database : ''}`)
  t.push(`${d.repo.replace(/^.*[:/]([^/:]+\/[^/]+?)(\.git)?$/, '$1')} @ ${d.branch}`)
  t.push(d.dir)
  if (d.strategy === 'releases') t.push('zero-downtime releases')
  return t
})

async function deploy(commit?: string) {
  err.value = null
  try {
    deployJob.value = (await api.post<{ job_id: string }>(`/api/projects/${enc.value}/deploy`, commit ? { commit } : {})).job_id
    tab.value = 'deploys'
  } catch (e) { fail(e) }
}
function onDeployDone() { setTimeout(() => { p.refresh(); ov.refresh(); rel.refresh() }, 400) }

// ------------------------------------------------------------------ artisan
const artisanCmd = ref('')
const artisanJob = ref<string | null>(null)
const quickArtisan = ['migrate', 'optimize', 'cache:clear', 'config:cache', 'queue:restart', 'storage:link']
async function runArtisan(cmd = artisanCmd.value.trim()) {
  if (!cmd) return
  err.value = null
  try {
    artisanJob.value = (await api.post<{ job_id: string }>(`/api/projects/${enc.value}/artisan`, { command: cmd })).job_id
    artisanCmd.value = ''
  } catch (e) { fail(e) }
}
const maintBusy = ref(false)
async function toggleMaintenance() {
  maintBusy.value = true
  tab.value = 'overview'
  try { await runArtisan(ov.data.value?.maintenance ? 'up' : 'down') } finally { maintBusy.value = false }
}
function onArtisanDone() { setTimeout(() => ov.refresh(), 500) }

// ------------------------------------------------------------------ health cards
function ago(ts: number | null | undefined) {
  if (!ts) return '—'
  const s = Math.max(0, Math.round(Date.now() / 1000 - ts))
  if (s < 90) return `${s} s ago`
  if (s < 5400) return `${Math.round(s / 60)} min ago`
  return `${Math.round(s / 3600)} h ago`
}
type Card = { name: string; value: string; state: string; tone: 'ok' | 'warn' | 'bad' | 'info' | 'off' }
const cards = computed<Card[]>(() => {
  const o = ov.data.value
  if (!o) return []
  const m = o.monitor
  const out: Card[] = []
  const up = m.uptime !== null ? `${(m.uptime * 100).toFixed(m.uptime === 1 ? 0 : 1)}% up · 24 h` : ''
  if (m.last_code === null) out.push({ name: 'HTTP check', value: '—', state: 'First check soon', tone: 'off' })
  else if (m.last_code === 0 || m.last_code >= 500) out.push({ name: 'HTTP check', value: m.last_code === 0 ? 'No answer' : String(m.last_code), state: up || 'Down', tone: 'bad' })
  else out.push({ name: 'HTTP check', value: `${m.last_code} · ${m.last_ms} ms`, state: up || `Every ${m.interval_secs} s`, tone: 'ok' })
  const d = o.site.ssl_days
  out.push(!o.site.ssl
    ? { name: 'SSL certificate', value: 'Off', state: 'Enable HTTPS', tone: 'warn' }
    : d === null ? { name: 'SSL certificate', value: 'On', state: 'Unknown expiry', tone: 'info' }
      : { name: 'SSL certificate', value: `${d} days`, state: d < 7 ? 'Expiring' : d < 14 ? 'Renew soon' : 'Valid', tone: d < 7 ? 'bad' : d < 14 ? 'warn' : 'ok' })
  if (isLaravel.value) {
    const pt = pendingTotal.value
    out.push(!o.workers.length
      ? { name: 'Queue pending', value: pt === null ? '—' : pt.toLocaleString(), state: 'No workers', tone: pt ? 'warn' : 'off' }
      : { name: 'Queue pending', value: pt === null ? '—' : pt.toLocaleString(), state: (pt ?? 0) > 500 ? 'Backlog' : 'Normal', tone: (pt ?? 0) > 500 ? 'warn' : 'ok' })
    const s = o.scheduler
    const late = s.last_run !== null && Date.now() / 1000 - s.last_run > 180
    out.push(!s.enabled
      ? { name: 'Scheduler', value: 'Off', state: 'Not running', tone: 'off' }
      : { name: 'Scheduler', value: ago(s.last_run), state: s.last_run === null ? 'Waiting' : late ? 'Late' : 'Running', tone: s.last_run === null ? 'info' : late ? 'warn' : 'ok' })
  } else {
    const e = phpEol.value
    out.push({ name: 'PHP version', value: o.site.php_version ?? '—', state: e?.label ?? '—', tone: e?.tone ?? 'off' })
    out.push({ name: 'Failed checks · 1 h', value: `${m.failures_1h} / ${m.checks_1h}`, state: m.failures_1h ? 'Errors' : 'Normal', tone: m.failures_1h ? 'warn' : 'ok' })
  }
  const n = o.errors.last_hour
  out.push({ name: 'Errors logged · 1 h', value: String(n), state: n === 0 ? 'Normal' : n > 50 ? 'High' : 'Some errors', tone: n === 0 ? 'ok' : n > 50 ? 'bad' : 'warn' })
  if (isLaravel.value) {
    const b = o.facts?.logs_bytes ?? null
    out.push({ name: 'storage/logs', value: b === null ? '—' : formatBytes(b), state: (b ?? 0) > 500e6 ? 'Rotate logs' : 'OK', tone: (b ?? 0) > 500e6 ? 'warn' : 'ok' })
  } else {
    const b = o.facts?.dir_bytes ?? null
    out.push({ name: 'Project size', value: b === null ? '—' : formatBytes(b), state: 'On disk', tone: 'ok' })
  }
  return out
})

// ------------------------------------------------------------------ chart
const chart = computed(() => {
  const b = ov.data.value?.monitor.buckets ?? []
  const vals = b.flatMap((x) => [x.p50, x.p95]).filter((v): v is number => v !== null)
  const max = Math.max(100, ...vals)
  const nice = [100, 200, 250, 500, 1000, 2000, 2500, 5000, 10000].find((n) => n >= max) ?? Math.ceil(max / 1000) * 1000
  const W = 600, H = 180, top = 14, bottom = 22
  const x = (i: number) => (b.length <= 1 ? 0 : (i / (b.length - 1)) * W)
  const y = (v: number) => top + (1 - v / nice) * (H - top - bottom)
  const path = (k: 'p50' | 'p95') => {
    let d = ''
    let pen = false
    b.forEach((pt, i) => {
      const v = pt[k]
      if (v === null) { pen = false; return }
      d += `${pen ? 'L' : 'M'}${x(i).toFixed(1)},${y(v).toFixed(1)} `
      pen = true
    })
    return d.trim()
  }
  const dots = (k: 'p50' | 'p95') => b
    .map((pt, i) => ({ x: x(i), v: pt[k], lone: pt[k] !== null && (b[i - 1]?.[k] ?? null) === null && (b[i + 1]?.[k] ?? null) === null }))
    .filter((d) => d.lone && d.v !== null)
    .map((d) => ({ x: d.x, y: y(d.v as number) }))
  const fails = b.map((pt, i) => ({ x: x(i), n: pt.failures })).filter((f) => f.n > 0)
  const grid = [nice, nice / 2, 0].map((v) => ({ y: y(v), label: v >= 1000 ? `${v / 1000} s` : `${v} ms` }))
  const hours = b.length ? [0, 6, 12, 18, b.length - 1].map((i) => ({ x: x(i), label: new Date(b[i]!.ts * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) })) : []
  return { p50: path('p50'), p95: path('p95'), dots50: dots('p50'), dots95: dots('p95'), fails, grid, hours }
})
const chartNote = computed(() => {
  const o = ov.data.value
  if (!o) return 'Loading response times'
  const b = o.monitor.buckets.filter((x) => x.p50 !== null)
  if (!b.length) return `Collecting data: NexaCloud checks the site every ${o.monitor.interval_secs} s; the first points appear within the hour.`
  const p50s = b.map((x) => x.p50 as number).sort((a, c) => a - c)
  const med = p50s[Math.floor(p50s.length / 2)]
  const worst = Math.max(...b.map((x) => x.p95 ?? 0))
  const failed = o.monitor.buckets.reduce((a, x) => a + x.failures, 0)
  return `Typical response ${med} ms, slowest hour p95 ${worst} ms.${failed ? ` ${failed} failed check${failed > 1 ? 's' : ''} in 24 h.` : ' No failed checks in 24 h.'}`
})

// ------------------------------------------------------------------ workers
const editingWorkers = ref(false)
const workerRows = ref<{ name: string; queue: string; processes: number }[]>([])
function startEditWorkers() {
  workerRows.value = (ov.data.value?.workers ?? []).map((w) => ({ name: w.name, queue: w.queue, processes: w.processes }))
  if (!workerRows.value.length) workerRows.value.push({ name: 'default', queue: 'default', processes: 2 })
  editingWorkers.value = true
}
function addWorkerRow() { workerRows.value.push({ name: `worker-${workerRows.value.length + 1}`, queue: 'default', processes: 1 }) }
function workersApplied(s: string) { if (s === 'applied') { editingWorkers.value = false; ov.refresh() } }
const restartBusy = ref<string | null>(null)
async function restartWorker(name?: string) {
  restartBusy.value = name ?? '*'
  try { await api.post(`/api/projects/${enc.value}/workers/restart`, name ? { name } : {}); setTimeout(() => ov.refresh(), 1500) } catch (e) { fail(e) } finally { restartBusy.value = null }
}
function workerTone(s: string) { return s === 'RUNNING' ? 'ok' : s === 'STARTING' ? 'info' : 'bad' }
function workerLabel(s: string) { return ({ RUNNING: 'Running', STARTING: 'Starting', BACKOFF: 'Crashing', FATAL: 'Stopped (error)', STOPPED: 'Stopped', EXITED: 'Exited', UNKNOWN: 'Not loaded' } as Record<string, string>)[s] ?? s }

// ------------------------------------------------------------------ schedule
function cronHuman(e: string) {
  const [mi, h, dom, mo, dow] = e.split(/\s+/)
  if (e === '* * * * *') return 'Every minute'
  if (/^\*\/\d+$/.test(mi ?? '') && h === '*' && dom === '*') return `Every ${mi!.slice(2)} min`
  if (mi === '0' && h === '*' && dom === '*') return 'Hourly'
  if (/^\d+$/.test(mi ?? '') && /^\d+$/.test(h ?? '') && dom === '*' && mo === '*') {
    const t = `${h!.padStart(2, '0')}:${mi!.padStart(2, '0')}`
    return dow === '*' ? `Daily ${t}` : `Weekly ${t}`
  }
  return e
}
function nextHuman(iso: string) {
  const s = Math.round((new Date(iso).getTime() - Date.now()) / 1000)
  if (Number.isNaN(s)) return '—'
  if (s < 90) return `in ${Math.max(s, 0)} s`
  if (s < 5400) return `in ${Math.round(s / 60)} min`
  if (s < 172800) return `in ${Math.round(s / 3600)} h`
  return formatDate(iso)
}

// ------------------------------------------------------------------ errors
function errWhen(ts: string) {
  const today = new Date().toISOString().slice(0, 10)
  return ts.startsWith(today) ? ts.slice(11, 16) : `${ts.slice(5, 10)} ${ts.slice(11, 16)}`
}
function errTone(l: string) { return ['ERROR', 'CRITICAL', 'ALERT', 'EMERGENCY'].includes(l) ? 'bad' : 'warn' }

// ------------------------------------------------------------------ env
const envData = ref<{ vars: EnvVar[]; exists: boolean; path: string } | null>(null)
const envEdits = reactive<Record<string, string | null>>({})
const editingKey = ref<string | null>(null)
const revealed = reactive<Record<string, string>>({})
const newKey = ref('')
const newVal = ref('')
async function loadEnv() { try { envData.value = await api.get(`/api/projects/${enc.value}/env`) } catch (e) { fail(e) } }
async function reveal(k: string) {
  if (revealed[k] !== undefined) { delete revealed[k]; return }
  try { revealed[k] = (await api.post<{ value: string }>(`/api/projects/${enc.value}/env/reveal`, { key: k })).value } catch (e) { fail(e) }
}
function editVar(v: EnvVar) {
  editingKey.value = v.key
  if (envEdits[v.key] === undefined) envEdits[v.key] = v.secret ? (revealed[v.key] ?? '') : (v.value ?? '')
}
function removeVar(k: string) { envEdits[k] = null; editingKey.value = null }
function undoVar(k: string) { delete envEdits[k]; if (editingKey.value === k) editingKey.value = null }
function addVar() {
  const k = newKey.value.trim().toUpperCase()
  if (!k) return
  envEdits[k] = newVal.value
  newKey.value = ''; newVal.value = ''
}
const envChanges = computed(() => ({ ...envEdits }))
const newKeys = computed(() => Object.keys(envEdits).filter((k) => !envData.value?.vars.some((v) => v.key === k)))
function envApplied(s: string) {
  if (s !== 'applied') return
  for (const k of Object.keys(envEdits)) delete envEdits[k]
  for (const k of Object.keys(revealed)) delete revealed[k]
  editingKey.value = null
  loadEnv()
}

// ------------------------------------------------------------------ logs
const logs = ref<{ sources: { id: string; label: string }[]; file: string | null; text: string } | null>(null)
const logFile = ref<string | null>(null)
const logFilter = ref('')
const live = ref(false)
let logTimer: ReturnType<typeof setInterval> | null = null
let filterTimer: ReturnType<typeof setTimeout> | null = null
const logBox = ref<HTMLElement | null>(null)
async function loadLogs() {
  const q = new URLSearchParams({ lines: '400' })
  if (logFile.value) q.set('file', logFile.value)
  if (logFilter.value.trim()) q.set('q', logFilter.value.trim())
  try {
    logs.value = await api.get(`/api/projects/${enc.value}/logs?${q}`)
    if (!logFile.value) logFile.value = logs.value?.file ?? null
    await nextTick()
    if (live.value && logBox.value) logBox.value.scrollTop = logBox.value.scrollHeight
  } catch (e) { fail(e) }
}
watch(logFilter, () => { if (filterTimer) clearTimeout(filterTimer); filterTimer = setTimeout(loadLogs, 350) })
watch(live, (on) => {
  if (logTimer) { clearInterval(logTimer); logTimer = null }
  if (on) { loadLogs(); logTimer = setInterval(loadLogs, 3000) }
})
function downloadLog() {
  const blob = new Blob([logs.value?.text ?? ''], { type: 'text/plain' })
  const a = document.createElement('a')
  a.href = URL.createObjectURL(blob)
  a.download = `${id.value}-${logFile.value ?? 'log'}.txt`
  a.click()
  setTimeout(() => URL.revokeObjectURL(a.href), 1000)
}
onBeforeUnmount(() => { if (logTimer) clearInterval(logTimer) })

watch(tab, (t) => {
  if (t === 'env' && !envData.value) loadEnv()
  if (t === 'logs') loadLogs()
  if (t !== 'logs') live.value = false
}, { immediate: true })

// ------------------------------------------------------------------ deploys
interface Release { name: string; current: boolean; commit: string; message: string; author: string; at: string }
const rel = useResource<{ strategy: string; releases: Release[] }>(`/api/projects/${enc.value}/releases`, { interval: 20_000 })
const selDeploy = ref<string | null>(null)
const shownDeploy = computed(() => {
  const list = p.data.value?.deploys ?? []
  return list.find((d) => d.id === selDeploy.value) ?? list[0] ?? null
})
const activating = ref<string | null>(null)
async function activate(name: string) {
  activating.value = name
  err.value = null
  try {
    deployJob.value = (await api.post<{ job_id: string }>(`/api/projects/${enc.value}/releases/${encodeURIComponent(name)}/activate`, {})).job_id
  } catch (e) { fail(e) } finally { activating.value = null }
}
function stepIcon(s: string) { return s === 'ok' ? '✓' : s === 'failed' ? '✕' : s === 'running' ? '…' : '–' }
function deployNote(d: DeployRow) {
  if (d.status === 'ok') return p.data.value?.strategy === 'releases' ? `Zero-downtime: release ${d.release ?? ''} went live only after the health check passed.` : 'Live. The health check passed.'
  if (d.status === 'rolled_back') return d.error ?? 'The site failed its health check, so the previous version is live again.'
  if (d.status === 'failed') return p.data.value?.strategy === 'releases' && !d.steps.some((s) => s.name === 'Switch release' && s.status === 'ok') ? 'Nothing changed on the live site.' : (d.error ?? '')
  return 'Running…'
}
function tone(s: string) { return s === 'ok' ? 'ok' : s === 'running' || s === 'queued' ? 'info' : 'bad' }
function label(s: string) { return ({ ok: 'Live', running: 'Deploying', queued: 'Queued', failed: 'Failed', rolled_back: 'Rolled back' } as Record<string, string>)[s] ?? s }
const panelOrigin = computed(() => (import.meta.client ? window.location.origin : ''))
async function copy(t: string) { await navigator.clipboard?.writeText(t) }

// ------------------------------------------------------------------ pipeline settings
const settings = reactive({ repo: '', branch: '', script: '', migrate: true, auto_deploy: false, run_tests: false, notify: true, keep_releases: 5 })
const settingsLoaded = ref(false)
watch(() => p.data.value, (v) => {
  if (v && !settingsLoaded.value) {
    Object.assign(settings, { repo: v.repo, branch: v.branch, script: v.script, migrate: v.migrate, auto_deploy: v.auto_deploy, run_tests: v.run_tests, notify: v.notify, keep_releases: v.keep_releases })
    settingsLoaded.value = true
  }
}, { immediate: true })
const saved = ref<string | null>(null)
async function save() {
  saved.value = null
  try { await api.put(`/api/projects/${enc.value}`, { ...settings }); saved.value = 'Saved.'; p.refresh() } catch (e) { if (!(e instanceof SudoCancelled)) saved.value = errorMessage(e) }
}
async function disconnect() {
  try { await api.del(`/api/projects/${enc.value}`); navigateTo('/projects') } catch (e) { fail(e) }
}
</script>

<template>
  <header class="phead">
    <div class="stack" style="gap: 10px">
      <p class="lbl"><NuxtLink to="/projects" class="crumb">Deployments</NuxtLink> / {{ id }}</p>
      <div class="row wrap" style="gap: 12px">
        <h1>{{ id }}</h1>
        <StatusPill :tone="health.tone" :label="health.label" />
      </div>
      <div class="row wrap" style="gap: 6px">
        <span v-for="t in headTags" :key="t" class="tag mono">{{ t }}</span>
      </div>
    </div>
    <div class="row wrap" style="gap: 10px">
      <a v-if="ov.data.value" class="btn" :href="ov.data.value.site.url" target="_blank" rel="noopener">Visit site</a>
      <button v-if="isLaravel && ov.data.value" type="button" class="btn" :disabled="maintBusy" :aria-pressed="ov.data.value.maintenance" @click="toggleMaintenance">
        Maintenance mode: {{ ov.data.value.maintenance ? 'On' : 'Off' }}
      </button>
      <button type="button" class="btn btn-p" :disabled="!p.data.value" @click="deploy()"><AppIcon name="play" :size="14" :stroke="2" /> Deploy now</button>
    </div>
  </header>
  <p v-if="err" class="small" style="color: var(--bad)" role="alert">{{ err }} <button type="button" class="linkish" @click="err = null">Dismiss</button></p>

  <nav class="tabs" aria-label="Project sections">
    <button v-for="t in tabs" :key="t[0]" type="button" :class="{ on: tab === t[0] }" :aria-pressed="tab === t[0]" @click="tab = t[0]">{{ t[1] }}</button>
  </nav>

  <!-- ================================================================ overview -->
  <div v-if="tab === 'overview'" class="stack" style="gap: 20px">
    <JobLog v-if="artisanJob" :key="artisanJob" :job-id="artisanJob" status="running" @finished="onArtisanDone" />

    <section aria-label="Health checks" class="checks">
      <template v-if="ov.data.value">
        <div v-for="c in cards" :key="c.name" class="card check">
          <span class="lbl">{{ c.name }}</span>
          <span class="check-val">{{ c.value }}</span>
          <StatusPill :tone="c.tone" :label="c.state" style="align-self: flex-start" />
        </div>
      </template>
      <template v-else>
        <div v-for="i in 6" :key="i" class="card check"><span class="skel" style="height: 12px; width: 60%" /><span class="skel" style="height: 22px; width: 40%" /><span class="skel" style="height: 18px; width: 50%" /></div>
      </template>
    </section>

    <div class="split">
      <section class="card chart-card" aria-labelledby="rt">
        <div class="row wrap" style="justify-content: space-between; gap: 10px">
          <h2 id="rt">Response time · 24 h</h2>
          <div class="row legend">
            <span><i style="background: #6CB4FF" />p50</span>
            <span><i style="background: #F7C55E" />p95</span>
            <span v-if="chart.fails.length"><i style="background: #FF9A9A; height: 8px; width: 3px" />failed checks</span>
          </div>
        </div>
        <svg viewBox="0 0 600 180" class="chart" role="img" :aria-label="chartNote">
          <g stroke="#1F2731" stroke-width="1"><line v-for="g in chart.grid" :key="g.y" x1="0" :y1="g.y" x2="600" :y2="g.y" /></g>
          <rect v-for="f in chart.fails" :key="f.x" :x="f.x - 2" y="150" width="4" height="8" fill="#FF9A9A" />
          <path v-if="chart.p50" :d="chart.p50" fill="none" stroke="#6CB4FF" stroke-width="2" stroke-linejoin="round" />
          <path v-if="chart.p95" :d="chart.p95" fill="none" stroke="#F7C55E" stroke-width="2" stroke-linejoin="round" />
          <circle v-for="d in chart.dots50" :key="'a' + d.x" :cx="d.x" :cy="d.y" r="3" fill="#6CB4FF" />
          <circle v-for="d in chart.dots95" :key="'b' + d.x" :cx="d.x" :cy="d.y" r="3" fill="#F7C55E" />
          <g font-family="Inter" font-size="11" fill="#A3AEBD">
            <text v-for="g in chart.grid.slice(0, 2)" :key="'t' + g.y" x="4" :y="g.y + 12">{{ g.label }}</text>
            <text v-for="(h, i) in chart.hours" :key="'h' + i" :x="Math.min(Math.max(h.x, 18), 582)" y="176" text-anchor="middle">{{ h.label }}</text>
          </g>
        </svg>
        <p class="small text-2">{{ chartNote }}</p>
      </section>

      <section v-if="isLaravel" class="card side-card" aria-labelledby="art">
        <h2 id="art">Artisan</h2>
        <div class="row wrap" style="gap: 8px">
          <button v-for="a in quickArtisan" :key="a" type="button" class="btn btn-s mono" style="font-size: 12px" @click="runArtisan(a)">{{ a }}</button>
        </div>
        <form class="artisan-in" @submit.prevent="runArtisan()">
          <span class="mono" style="font-size: 12px; color: #5BE6A9">php artisan</span>
          <input v-model="artisanCmd" class="mono" type="text" list="artisan-cmds" aria-label="Artisan command" placeholder="migrate:status, about, view:clear…" autocomplete="off">
          <datalist id="artisan-cmds"><option v-for="c in status.data.value?.artisan ?? []" :key="c" :value="c" /></datalist>
          <button type="submit" class="btn btn-s btn-p" :disabled="!artisanCmd.trim()">Run</button>
        </form>
        <p class="small muted">Runs as {{ p.data.value?.run_as }} in <span class="mono">{{ ov.data.value?.app_dir }}</span>. Only safe commands are allowed.</p>
      </section>

      <section v-else-if="p.data.value" class="card side-card" aria-labelledby="phpset">
        <h2 id="phpset">PHP project settings</h2>
        <div v-if="phpEol && (phpEol.tone === 'bad' || phpEol.tone === 'warn')" class="eol">
          <StatusPill :tone="phpEol.tone" :label="`PHP ${ov.data.value?.site.php_version}: ${phpEol.label}`" style="align-self: flex-start" />
          <span class="small text-2">Move this site to a supported PHP version. NexaCloud checks the site after switching and rolls back if it breaks.</span>
          <NuxtLink class="btn btn-s btn-p" :to="`/sites?q=${encodeURIComponent(p.data.value.site_id)}`" style="align-self: flex-start">Review &amp; upgrade</NuxtLink>
        </div>
        <dl class="facts">
          <dt>Project folder</dt><dd class="mono">{{ p.data.value.dir }}</dd>
          <dt>PHP</dt><dd>{{ ov.data.value?.site.php_version ?? '—' }} · {{ ov.data.value?.site.runtime === 'frankenphp' ? 'FrankenPHP' : 'PHP-FPM' }}</dd>
          <dt>Runs as</dt><dd class="mono">{{ p.data.value.run_as }}</dd>
          <dt>On deploy</dt><dd>{{ p.data.value.script.includes('composer') ? 'composer install, then your script' : 'your script' }}</dd>
        </dl>
        <NuxtLink class="btn btn-s" :to="`/sites?q=${encodeURIComponent(p.data.value.site_id)}`" style="align-self: flex-start">Site settings (web root, PHP, SSL)</NuxtLink>
      </section>
    </div>

    <section v-if="isLaravel && ov.data.value" class="card" aria-labelledby="wk" style="overflow: hidden">
      <div class="row wrap card-top">
        <h2 id="wk">Queue workers · Supervisor</h2>
        <div class="row" style="gap: 8px">
          <button type="button" class="btn btn-s" @click="editingWorkers ? (editingWorkers = false) : startEditWorkers()">{{ editingWorkers ? 'Close' : ov.data.value.workers.length ? 'Edit workers' : 'Add worker' }}</button>
          <button v-if="ov.data.value.workers.length" type="button" class="btn btn-s btn-p" :disabled="restartBusy !== null" @click="restartWorker()">Restart all</button>
        </div>
      </div>
      <div v-if="editingWorkers" class="stack wedit">
        <div v-for="(w, i) in workerRows" :key="i" class="wrow">
          <label class="field"><span>Name</span><input v-model="w.name" class="in mono" type="text"></label>
          <label class="field"><span>Queues (comma separated)</span><input v-model="w.queue" class="in mono" type="text"></label>
          <label class="field"><span>Processes</span><input v-model.number="w.processes" class="in" type="number" min="1" max="8"></label>
          <button type="button" class="btn btn-s" style="align-self: end" :aria-label="`Remove worker ${w.name}`" @click="workerRows.splice(i, 1)">Remove</button>
        </div>
        <div class="row wrap" style="gap: 8px">
          <button type="button" class="btn btn-s" @click="addWorkerRow">Add another worker</button>
        </div>
        <PlanAction
          v-if="workerRows.length"
          :key="JSON.stringify(workerRows)"
          :request="{ kind: 'project_worker', params: { id, enabled: true, workers: workerRows } }"
          label="Preview workers"
          success-text="Workers are running under Supervisor."
          @done="workersApplied"
        />
        <PlanAction
          v-else-if="ov.data.value.workers.length"
          :request="{ kind: 'project_worker', params: { id, enabled: false } }"
          label="Preview removing all workers"
          success-text="Workers removed."
          @done="workersApplied"
        />
      </div>
      <div v-if="!ov.data.value.workers.length && !editingWorkers" class="empty"><strong>No queue workers</strong><span>Jobs pushed to the queue wait until a worker runs them. Add one: NexaCloud runs it under Supervisor and restarts it after every deploy.</span></div>
      <div v-else-if="ov.data.value.workers.length" class="table-wrap">
        <table>
          <thead><tr><th>Worker</th><th>Queue</th><th>Processes</th><th>Pending jobs</th><th>Failed 24 h</th><th>Status</th><th>Uptime</th><th><span class="sr-only">Actions</span></th></tr></thead>
          <tbody>
            <tr v-for="w in ov.data.value.workers" :key="w.name">
              <td class="mono" style="font-size: 13px">{{ w.name }}</td>
              <td><span class="tag mono">{{ w.queue }}</span></td>
              <td class="mono">{{ w.running }}/{{ w.processes }}</td>
              <td class="mono">{{ w.pending ?? '—' }}</td>
              <td class="mono">{{ w.failed_24h ?? '—' }}</td>
              <td><StatusPill :tone="workerTone(w.state)" :label="workerLabel(w.state)" /></td>
              <td class="small text-2">{{ w.uptime ?? '—' }}</td>
              <td><button type="button" class="btn btn-s" :disabled="restartBusy !== null" @click="restartWorker(w.name)">Restart</button></td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <div class="split">
      <section v-if="isLaravel && ov.data.value" class="card half" aria-labelledby="sch" style="overflow: hidden">
        <div class="row wrap card-top">
          <h2 id="sch">Scheduler</h2>
          <PlanAction
            :key="String(ov.data.value.scheduler.enabled)"
            :request="{ kind: 'project_scheduler', params: { id, enabled: !ov.data.value.scheduler.enabled } }"
            :label="ov.data.value.scheduler.enabled ? 'Turn off' : 'Turn on'"
            success-text="Done."
            @done="ov.refresh()"
          />
        </div>
        <div v-if="!(ov.data.value.facts.schedule ?? []).length" class="empty" style="padding: 20px">
          <span v-if="ov.data.value.facts.error">{{ ov.data.value.facts.error }}</span>
          <span v-else>No scheduled commands defined in the app.</span>
        </div>
        <div v-else class="table-wrap">
          <table>
            <thead><tr><th>Command</th><th>Runs</th><th>Next run</th></tr></thead>
            <tbody>
              <tr v-for="s in ov.data.value.facts.schedule" :key="s.command + s.expression">
                <td class="mono" style="font-size: 13px; white-space: normal">{{ s.command }}</td>
                <td class="small text-2">{{ cronHuman(s.expression) }}</td>
                <td class="small text-2">{{ ov.data.value.scheduler.enabled ? nextHuman(s.next) : 'Scheduler off' }}</td>
              </tr>
            </tbody>
          </table>
        </div>
        <p class="small muted" style="padding: 12px 20px">{{ ov.data.value.scheduler.enabled ? `cron runs schedule:run every minute as ${p.data.value?.run_as}. Last run ${ago(ov.data.value.scheduler.last_run)}.` : 'Turn on to run php artisan schedule:run every minute.' }}</p>
      </section>

      <section v-if="ov.data.value" class="card half errs" aria-labelledby="err">
        <div class="row" style="justify-content: space-between; gap: 10px">
          <h2 id="err">Latest errors</h2>
          <span class="lbl mono">{{ ov.data.value.errors.file }}</span>
        </div>
        <div v-if="!ov.data.value.errors.items.length" class="empty" style="padding: 20px"><span>No errors or warnings in the log.</span></div>
        <div v-for="e in ov.data.value.errors.items" :key="e.msg" class="err-item">
          <div class="row wrap" style="gap: 8px"><StatusPill :tone="errTone(e.level)" :label="e.level" /><span class="small muted">{{ errWhen(e.when) }} · {{ e.count }}×</span></div>
          <span class="mono err-msg">{{ e.msg }}</span>
        </div>
        <button type="button" class="btn btn-s" style="align-self: flex-start" @click="tab = 'logs'">Open logs</button>
      </section>
    </div>
  </div>

  <!-- ================================================================ deploys -->
  <template v-if="tab === 'deploys' && p.data.value">
    <section v-if="deployJob" class="card card-pad stack" style="gap: 8px">
      <strong>Deploying…</strong>
      <JobLog :key="deployJob" :job-id="deployJob" status="running" @finished="onDeployDone" />
    </section>
    <div v-if="!p.data.value.deploys.length" class="card empty"><strong>No deploys yet</strong><span>Click “Deploy now”. The first deploy checks out the repository into {{ p.data.value.dir }}.</span></div>
    <div v-else class="split">
      <section class="card runs" aria-labelledby="runs-h">
        <h2 id="runs-h" style="padding: 10px 12px 8px">Recent runs</h2>
        <button v-for="(d, i) in p.data.value.deploys" :key="d.id" type="button" class="run" :class="{ on: shownDeploy?.id === d.id }" :aria-pressed="shownDeploy?.id === d.id" @click="selDeploy = d.id">
          <StatusPill :tone="tone(d.status)" :label="label(d.status)" />
          <span class="grow" style="display: flex; flex-direction: column; gap: 2px; min-width: 0">
            <span style="font-size: 14px; font-weight: 600">#{{ p.data.value.deploys.length - i }} <span class="muted" style="font-weight: 500">{{ d.trigger }}</span></span>
            <span class="mono small muted ellipsis">{{ d.commit?.slice(0, 8) ?? '—' }} · {{ d.message ?? '' }}</span>
          </span>
          <span class="small muted" style="white-space: nowrap">{{ formatRelative(d.started_at) }}</span>
        </button>
      </section>
      <section v-if="shownDeploy" class="card detail" aria-labelledby="rd">
        <div class="row wrap" style="justify-content: space-between; gap: 12px; align-items: flex-start">
          <div class="stack" style="gap: 6px">
            <div class="row wrap" style="gap: 10px"><h2 id="rd" style="font-size: 20px">{{ id }}<template v-if="shownDeploy.release"> · release {{ shownDeploy.release }}</template></h2><StatusPill :tone="tone(shownDeploy.status)" :label="label(shownDeploy.status)" /></div>
            <span class="mono small muted">{{ p.data.value.branch }} · {{ shownDeploy.commit?.slice(0, 8) ?? '—' }} · {{ shownDeploy.message ?? '' }}<template v-if="shownDeploy.author"> · by {{ shownDeploy.author }}</template></span>
          </div>
          <div class="row wrap" style="gap: 8px">
            <NuxtLink v-if="shownDeploy.job_id" :to="`/activity?open=${shownDeploy.job_id}`" class="btn btn-s">Full log</NuxtLink>
            <ConfirmAction v-if="shownDeploy.status === 'ok' && shownDeploy.commit && shownDeploy.commit !== p.data.value.last_commit" label="Roll back to this" :question="`Make ${shownDeploy.commit.slice(0, 8)} live again?`" confirm-label="Roll back" @confirm="deploy(shownDeploy.commit!)" />
            <button v-else-if="shownDeploy.commit && shownDeploy.status !== 'running'" type="button" class="btn btn-s" @click="deploy()">Redeploy</button>
          </div>
        </div>
        <ol v-if="shownDeploy.steps.length" class="steps">
          <li v-for="(st, i) in shownDeploy.steps" :key="i">
            <span class="sicon" :class="st.status" aria-hidden="true">{{ stepIcon(st.status) }}</span>
            <span class="grow" style="display: flex; flex-direction: column; gap: 2px; min-width: 0"><span style="font-size: 14px; font-weight: 600">{{ st.name }}</span><span class="mono small muted ellipsis">{{ st.cmd }}</span></span>
            <span class="small text-2">{{ st.result || st.status }}</span>
            <span class="mono small muted" style="width: 52px; text-align: right">{{ st.secs !== null ? `${st.secs}s` : '—' }}</span>
          </li>
        </ol>
        <p v-else class="small muted">Step details are recorded for deploys made with this version of NexaCloud. Open the full log for older ones.</p>
        <p v-if="shownDeploy.error && shownDeploy.status !== 'rolled_back'" class="small" style="color: var(--warn)">{{ shownDeploy.error }}</p>
        <p class="small text-2">{{ deployNote(shownDeploy) }}</p>
      </section>
    </div>
    <section v-if="p.data.value.strategy === 'releases'" class="card card-pad stack" aria-labelledby="rel-h" style="gap: 6px">
      <h2 id="rel-h">Releases on disk</h2>
      <p class="small muted">Activating an older release only switches the <span class="mono">current</span> link and checks the site — seconds, no rebuild. Database migrations are not undone.</p>
      <div v-for="r in rel.data.value?.releases ?? []" :key="r.name" class="list-row">
        <span class="mono grow" style="font-size: 13px">releases/{{ r.name }}</span>
        <span class="mono small muted">{{ r.commit?.slice(0, 8) }}</span>
        <span class="small muted ellipsis" style="max-width: 260px">{{ r.message }}</span>
        <StatusPill v-if="r.current" tone="ok" label="current" />
        <ConfirmAction v-else :label="activating === r.name ? 'Activating…' : 'Activate'" :question="`Make release ${r.name} live?`" confirm-label="Activate" :busy="activating === r.name" @confirm="activate(r.name)" />
      </div>
      <div v-if="!(rel.data.value?.releases ?? []).length" class="small muted">No releases yet — the first deploy creates releases/1.</div>
    </section>
  </template>

  <!-- ================================================================ env -->
  <section v-if="tab === 'env'" class="card card-pad stack" aria-label="Environment variables" style="gap: 14px">
    <div class="stack" style="gap: 4px">
      <h2>.env</h2>
      <p class="small muted">Secrets stay hidden until you reveal one (asks for your password and is logged). Saving clears Laravel's config cache and checks the site; if it breaks, the old file is restored.</p>
    </div>
    <p v-if="envData && !envData.exists" class="note">No .env file yet. Add variables below and save to create it.</p>
    <div class="table-wrap">
      <table>
        <thead><tr><th>Key</th><th>Value</th><th><span class="sr-only">Actions</span></th></tr></thead>
        <tbody>
          <tr v-for="v in envData?.vars ?? []" :key="v.key" :class="{ changed: envEdits[v.key] !== undefined }">
            <td class="mono env-k">{{ v.key }}</td>
            <td class="mono env-v">
              <input v-if="editingKey === v.key && envEdits[v.key] !== null" v-model="envEdits[v.key]" class="in mono" type="text" :aria-label="`Value of ${v.key}`" @keydown.enter="editingKey = null">
              <span v-else-if="envEdits[v.key] === null" class="muted">removed</span>
              <span v-else-if="envEdits[v.key] !== undefined">{{ v.secret ? '•••••• (new value)' : envEdits[v.key] }}</span>
              <span v-else-if="v.secret">{{ revealed[v.key] ?? '••••••••••••' }}</span>
              <span v-else>{{ v.value }}</span>
            </td>
            <td><div class="row" style="gap: 6px; justify-content: flex-end">
              <button v-if="envEdits[v.key] !== undefined" type="button" class="btn btn-s" @click="undoVar(v.key)">Undo</button>
              <template v-else>
                <button v-if="v.secret" type="button" class="btn btn-s" @click="reveal(v.key)">{{ revealed[v.key] !== undefined ? 'Hide' : 'Reveal' }}</button>
                <button type="button" class="btn btn-s" @click="editVar(v)">Edit</button>
                <button type="button" class="btn btn-s btn-ghost" :aria-label="`Remove ${v.key}`" @click="removeVar(v.key)">Remove</button>
              </template>
            </div></td>
          </tr>
          <tr v-for="k in newKeys" :key="'n' + k" class="changed">
            <td class="mono env-k">{{ k }} <span class="tag">new</span></td>
            <td class="mono env-v"><input v-model="envEdits[k]" class="in mono" type="text" :aria-label="`Value of ${k}`"></td>
            <td><div class="row" style="justify-content: flex-end"><button type="button" class="btn btn-s" @click="undoVar(k)">Undo</button></div></td>
          </tr>
        </tbody>
      </table>
    </div>
    <form class="env-add" @submit.prevent="addVar">
      <input v-model="newKey" class="in mono" type="text" placeholder="NEW_KEY" aria-label="New variable name">
      <input v-model="newVal" class="in mono" type="text" placeholder="value" aria-label="New variable value">
      <button type="submit" class="btn btn-s" :disabled="!newKey.trim()">Add variable</button>
    </form>
    <PlanAction :key="JSON.stringify(envChanges)" :request="{ kind: 'project_env', params: { id, set: envChanges } }" label="Preview & save" :disabled="!Object.keys(envChanges).length" success-text=".env saved and the site still works." @done="envApplied" />
  </section>

  <!-- ================================================================ logs -->
  <section v-if="tab === 'logs'" class="card card-pad stack" aria-label="Logs" style="gap: 14px">
    <div class="row wrap" style="gap: 10px">
      <select v-model="logFile" class="in" aria-label="Log source" style="width: auto" @change="loadLogs">
        <option v-for="s in logs?.sources ?? []" :key="s.id" :value="s.id">{{ s.label }}</option>
      </select>
      <input v-model="logFilter" class="in" type="search" aria-label="Filter logs" placeholder="Filter: ERROR, user_id=…" style="flex: 1; min-width: 200px">
      <button type="button" class="btn btn-s" :class="{ 'btn-on': live }" :aria-pressed="live" @click="live = !live">
        <span class="dot" :class="{ on: live }" aria-hidden="true" /> {{ live ? 'Live tail on' : 'Live tail' }}
      </button>
      <button type="button" class="btn btn-s" @click="loadLogs">Refresh</button>
      <button type="button" class="btn btn-s" :disabled="!logs?.text" @click="downloadLog">Download</button>
    </div>
    <pre ref="logBox" class="logview mono">{{ logs?.text || (logFilter ? 'No lines match the filter.' : 'This log is empty.') }}</pre>
  </section>

  <!-- ================================================================ pipeline -->
  <section v-if="tab === 'pipeline' && p.data.value" class="card card-pad stack" style="gap: 14px">
    <div class="fgrid">
      <label class="field"><span>Repository</span><input v-model="settings.repo" class="in mono" type="text"></label>
      <label class="field"><span>Branch</span><input v-model="settings.branch" class="in mono" type="text"></label>
    </div>
    <div class="toggles">
      <label class="tog"><span class="grow"><strong>Deploy when I push</strong><span class="small muted">GitHub webhook, or a check every minute</span></span><input v-model="settings.auto_deploy" type="checkbox"></label>
      <label v-if="isLaravel" class="tog"><span class="grow"><strong>Stop if tests fail</strong><span class="small muted">Runs php artisan test before going live</span></span><input v-model="settings.run_tests" type="checkbox"></label>
      <label v-if="isLaravel" class="tog"><span class="grow"><strong>Back up database before migrate</strong><span class="small muted">Quick dump, kept with the backups</span></span><input v-model="settings.migrate" type="checkbox"></label>
      <label class="tog"><span class="grow"><strong>Notify on failure</strong><span class="small muted">Telegram or email, set up under Monitoring</span></span><input v-model="settings.notify" type="checkbox"></label>
      <label v-if="p.data.value.strategy === 'releases'" class="tog"><span class="grow"><strong>Releases kept on disk</strong><span class="small muted">Older ones are removed after a good deploy</span></span><input v-model.number="settings.keep_releases" class="in" type="number" min="1" max="20" style="width: 80px"></label>
    </div>
    <div v-if="p.data.value.strategy !== 'releases'" class="convert">
      <div class="stack" style="gap: 4px"><strong>Switch to zero-downtime releases</strong><span class="small muted">Copies the current code to releases/1 and .env + storage/ to shared/, then serves current/. From then on deploys build in a new folder and go live only when they work. Your files stay where they are.</span></div>
      <PlanAction :request="{ kind: 'project_releases', params: { id } }" label="Preview" apply-label="Switch to releases" success-text="Now using release folders." @done="(s) => { if (s === 'applied') { p.refresh(); rel.refresh() } }" />
    </div>
    <p class="small text-2">Strategy: <strong>{{ p.data.value.strategy === 'releases' ? 'Zero-downtime releases' : 'In place' }}</strong>. {{ p.data.value.strategy === 'releases' ? 'Each deploy builds in a new release folder; the site switches to it only after the build and health check pass.' : 'Code is updated in the project folder; a failed health check restores the previous commit.' }}</p>
    <label class="field"><span>Deploy script (runs as {{ p.data.value.run_as }} {{ p.data.value.strategy === 'releases' ? 'in the new release folder' : `in ${p.data.value.dir}` }}, after the code is checked out)</span>
      <textarea v-model="settings.script" class="in mono script" rows="10" spellcheck="false" />
    </label>
    <div class="row wrap" style="gap: 10px"><button type="button" class="btn btn-p" @click="save">Save</button><span v-if="saved" class="small" role="status">{{ saved }}</span>
      <ConfirmAction label="Disconnect" question="Disconnect this site from Git? The site and its files stay." confirm-label="Disconnect" @confirm="disconnect" /></div>
  </section>

  <!-- ================================================================ setup -->
  <template v-if="tab === 'setup' && p.data.value">
    <section v-if="p.data.value.needs_key" class="card card-pad stack" style="gap: 10px">
      <h2>1. Add the deploy key to GitHub</h2>
      <p class="small text-2">GitHub → your repository → <strong>Settings → Deploy keys → Add deploy key</strong>. Paste this key, leave “Allow write access” off.</p>
      <div class="code-row"><code class="mono">{{ p.data.value.deploy_key }}</code><button type="button" class="btn btn-s" @click="copy(p.data.value.deploy_key ?? '')">Copy</button></div>
    </section>
    <section class="card card-pad stack" style="gap: 10px">
      <h2>{{ p.data.value.needs_key ? '2. ' : '' }}Deploy on push</h2>
      <p class="small text-2"><strong>Automatic (already works):</strong> with “Deploy when I push” on, NexaCloud checks GitHub every minute and deploys new commits.</p>
      <p class="small text-2"><strong>Instant (webhook):</strong> GitHub → Settings → Webhooks → Add webhook. Content type <span class="mono">application/json</span>, “Just the push event”. GitHub needs a valid HTTPS certificate, so give the panel its own domain first (Security page).</p>
      <div class="kv"><span class="lbl">Payload URL</span><div class="code-row"><code class="mono">{{ panelOrigin }}{{ p.data.value.webhook_path }}</code><button type="button" class="btn btn-s" @click="copy(panelOrigin + p.data.value.webhook_path)">Copy</button></div></div>
      <div class="kv"><span class="lbl">Secret</span><div class="code-row"><code class="mono">{{ p.data.value.webhook_secret }}</code><button type="button" class="btn btn-s" @click="copy(p.data.value.webhook_secret)">Copy</button></div></div>
    </section>
    <section class="card card-pad stack" style="gap: 10px">
      <h2>Prefer GitHub Actions?</h2>
      <p class="small text-2">Add this file as <span class="mono">.github/workflows/deploy.yml</span> and the repository secrets <span class="mono">NEXACLOUD_URL</span> and <span class="mono">NEXACLOUD_SECRET</span>. Tests run on GitHub; only a green build triggers the deploy. (Then turn off “Deploy when I push” here.)</p>
      <pre class="yaml mono">{{ p.data.value.actions_yaml }}</pre>
      <button type="button" class="btn btn-s" style="align-self: flex-start" @click="copy(p.data.value.actions_yaml)">Copy file</button>
    </section>
  </template>
</template>

<style scoped>
.phead { display: flex; flex-wrap: wrap; align-items: flex-start; justify-content: space-between; gap: 16px; }
.phead h1 { font-size: 30px; font-weight: 600; margin: 0; }
.crumb { color: var(--muted); }
h2 { font-size: 17px; font-weight: 600; margin: 0; }
.linkish { background: none; border: 0; color: var(--info); cursor: pointer; text-decoration: underline; font: inherit; }
.tabs { display: flex; flex-wrap: wrap; gap: 4px; border-bottom: 1px solid #222A34; }
.tabs button { min-height: 44px; padding: 0 16px; background: transparent; border: 0; border-bottom: 2px solid transparent; color: var(--muted); font-size: 14px; font-weight: 600; cursor: pointer; margin-bottom: -1px; }
.tabs button.on { color: var(--text); border-bottom-color: var(--accent); }
.checks { display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 12px; }
.check { padding: 14px 16px; display: flex; flex-direction: column; gap: 8px; }
.check-val { font-family: var(--font-head); font-size: 22px; font-weight: 600; }
.chart-card { flex: 2 1 520px; min-width: 0; padding: 18px 20px; display: flex; flex-direction: column; gap: 12px; }
.side-card { flex: 1 1 300px; min-width: 0; padding: 18px 20px; display: flex; flex-direction: column; gap: 12px; }
.half { flex: 1 1 420px; min-width: 0; }
.chart { width: 100%; height: 180px; }
.legend { gap: 14px; font-size: 12px; color: var(--muted); }
.legend span { display: flex; align-items: center; gap: 6px; }
.legend i { display: inline-block; width: 14px; height: 3px; }
.text-2 { color: var(--text-2, #C5CEDA); }
.artisan-in { display: flex; align-items: center; gap: 8px; min-height: 44px; padding: 0 4px 0 12px; background: #0F1318; border: 1px solid #2A3441; border-radius: 8px; }
.artisan-in input { flex: 1; min-width: 0; background: transparent; border: 0; outline: none; color: var(--text); font-size: 13px; min-height: 40px; }
.artisan-in:focus-within { border-color: var(--accent); }
.eol { padding: 12px; border-radius: 10px; background: #1A1012; border: 1px solid #4A2328; display: flex; flex-direction: column; gap: 8px; }
.facts { display: grid; grid-template-columns: auto 1fr; gap: 8px 14px; margin: 0; font-size: 13px; }
.facts dt { color: var(--muted); }
.facts dd { margin: 0; overflow-wrap: anywhere; }
.card-top { justify-content: space-between; gap: 10px; padding: 16px 20px; }
.wedit { padding: 0 20px 16px; gap: 10px; }
.wrow { display: grid; grid-template-columns: minmax(120px, 1fr) minmax(160px, 2fr) 110px auto; gap: 10px; }
@media (max-width: 640px) { .wrow { grid-template-columns: 1fr 1fr; } }
.errs { padding: 16px 20px; display: flex; flex-direction: column; gap: 10px; }
.err-item { padding: 12px; background: #0F1318; border: 1px solid #222A34; border-radius: 10px; display: flex; flex-direction: column; gap: 6px; }
.err-msg { font-size: 12px; color: var(--text); white-space: normal; word-break: break-word; }
.env-k { font-size: 13px; color: #8CC4FF; }
.env-v { font-size: 13px; white-space: normal; word-break: break-all; min-width: 200px; }
tr.changed td { background: rgba(61, 220, 151, .05); }
.env-add { display: grid; grid-template-columns: minmax(140px, 260px) 1fr auto; gap: 10px; }
@media (max-width: 640px) { .env-add { grid-template-columns: 1fr; } }
.logview { margin: 0; background: #07090C; border: 1px solid #1B222B; border-radius: 10px; padding: 16px; font-size: 12px; line-height: 1.8; color: var(--text-2, #C5CEDA); white-space: pre-wrap; word-break: break-word; overflow: auto; max-height: 65vh; min-height: 200px; }
.dot { width: 8px; height: 8px; border-radius: 50%; background: var(--muted); display: inline-block; }
.dot.on { background: var(--ok); box-shadow: 0 0 0 3px rgba(61, 220, 151, .2); }
.btn-on { border-color: var(--accent); }
.fgrid { display: grid; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); gap: 12px; }
.field { display: flex; flex-direction: column; gap: 6px; }
.field > span:first-child { font-size: 13px; font-weight: 500; color: var(--text-2); }
.in { min-height: 40px; padding: 0 10px; background: var(--sunken, #0F1318); border: 1px solid var(--btn-line, #2A3441); border-radius: 8px; color: var(--text); font-size: 13px; width: 100%; box-sizing: border-box; }
.script { padding: 10px; min-height: 180px; line-height: 1.6; }
input[type=checkbox] { accent-color: var(--accent); width: 20px; height: 20px; }
.toggles { display: flex; flex-direction: column; }
.tog { display: flex; align-items: center; gap: 12px; padding: 10px 0; border-bottom: 1px solid #1B222B; cursor: pointer; }
.tog .grow { display: flex; flex-direction: column; gap: 2px; font-size: 14px; }
.code-row { display: flex; gap: 8px; align-items: center; background: #07090C; border: 1px solid #1B222B; border-radius: 10px; padding: 8px 10px; }
.code-row code { flex: 1; font-size: 12px; overflow-wrap: anywhere; color: var(--text-2); }
.kv { display: flex; flex-direction: column; gap: 4px; }
.yaml { margin: 0; background: #07090C; border: 1px solid #1B222B; border-radius: 10px; padding: 12px 14px; font-size: 12px; line-height: 1.6; color: var(--text-2); overflow-x: auto; white-space: pre; }
.runs { flex: 1 1 340px; min-width: 0; padding: 8px; display: flex; flex-direction: column; gap: 2px; max-height: 640px; overflow: auto; }
.run { display: flex; align-items: center; gap: 12px; padding: 12px; min-height: 60px; border-radius: 9px; border: 1px solid transparent; background: transparent; color: var(--text); cursor: pointer; text-align: left; font: inherit; }
.run:hover { background: #151B22; }
.run.on { background: #1B2430; border-color: #2E3B4B; }
.detail { flex: 2 1 560px; min-width: 0; padding: 20px; display: flex; flex-direction: column; gap: 16px; }
.steps { list-style: none; margin: 0; padding: 0; }
.steps li { display: flex; align-items: center; gap: 12px; padding: 10px 0; border-bottom: 1px solid #1B222B; }
.sicon { width: 26px; height: 26px; border-radius: 50%; display: grid; place-items: center; font-size: 13px; font-weight: 700; flex: none; background: #1B222B; color: var(--muted); }
.sicon.ok { background: rgba(61, 220, 151, .14); color: #5BE6A9; }
.sicon.failed { background: rgba(255, 122, 122, .16); color: #FF9A9A; }
.sicon.running { background: rgba(108, 180, 255, .14); color: #8CC4FF; }
.ellipsis { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.convert { display: flex; flex-direction: column; gap: 10px; padding: 14px; border: 1px solid var(--card-line); border-radius: 10px; }
</style>
