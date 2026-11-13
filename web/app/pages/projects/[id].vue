<script setup lang="ts">
const route = useRoute()
const api = useApi()
const id = computed(() => String(route.params.id))
useHead({ title: () => `${id.value} · Deployments · NexaCloud` })

interface DeployRow { id: string; job_id: string | null; status: string; trigger: string; commit: string | null; message: string | null; author: string | null; previous_commit: string | null; started_at: string; finished_at: string | null; error: string | null }
interface ProjectDetail {
  id: string; site_id: string; kind: string; dir: string; repo: string; branch: string; run_as: string; script: string
  migrate: boolean; auto_deploy: boolean; webhook_secret: string; webhook_path: string; deploy_key: string | null; needs_key: boolean
  actions_yaml: string; last_commit: string | null; deploys: DeployRow[]
}
interface Status { worker: boolean; worker_status: string; scheduler: boolean; maintenance: boolean; artisan: string[] }
interface EnvVar { key: string; value: string | null; secret: boolean }

const p = useResource<ProjectDetail>(`/api/projects/${encodeURIComponent(id.value)}`, { interval: 10_000 })
const status = useResource<Status>(`/api/projects/${encodeURIComponent(id.value)}/status`)
const tab = ref<'deploys' | 'setup' | 'settings' | 'app' | 'env' | 'logs'>(route.query.new ? 'setup' : 'deploys')
const err = ref<string | null>(null)
const deployJob = ref<string | null>(null)
const isLaravel = computed(() => p.data.value?.kind === 'laravel')

async function deploy(commit?: string) {
  err.value = null
  try {
    deployJob.value = (await api.post<{ job_id: string }>(`/api/projects/${encodeURIComponent(id.value)}/deploy`, commit ? { commit } : {})).job_id
    tab.value = 'deploys'
  } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) }
}
function onDeployDone() { setTimeout(() => p.refresh(), 400) }
function tone(s: string) { return s === 'ok' ? 'ok' : s === 'running' || s === 'queued' ? 'info' : 'bad' }
function label(s: string) { return ({ ok: 'Live', running: 'Deploying', queued: 'Queued', failed: 'Failed', rolled_back: 'Rolled back' } as Record<string, string>)[s] ?? s }
const panelOrigin = computed(() => (import.meta.client ? window.location.origin : ''))
async function copy(t: string) { await navigator.clipboard?.writeText(t) }

// settings
const settings = reactive({ repo: '', branch: '', script: '', migrate: true, auto_deploy: false })
watch(() => p.data.value, (v) => {
  if (v && !settings.repo) Object.assign(settings, { repo: v.repo, branch: v.branch, script: v.script, migrate: v.migrate, auto_deploy: v.auto_deploy })
}, { immediate: true })
const saved = ref<string | null>(null)
async function save() {
  try { await api.put(`/api/projects/${encodeURIComponent(id.value)}`, { ...settings }); saved.value = 'Saved.'; p.refresh() } catch (e) { if (!(e instanceof SudoCancelled)) saved.value = errorMessage(e) }
}
async function disconnect() {
  try { await api.del(`/api/projects/${encodeURIComponent(id.value)}`); navigateTo('/projects') } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) }
}

// artisan
const artisanCmd = ref('optimize')
const artisanJob = ref<string | null>(null)
async function runArtisan(cmd = artisanCmd.value) {
  try { artisanJob.value = (await api.post<{ job_id: string }>(`/api/projects/${encodeURIComponent(id.value)}/artisan`, { command: cmd })).job_id } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) }
}
const workerForm = reactive({ processes: 2, queue: 'default' })

// env
const envData = ref<{ vars: EnvVar[]; exists: boolean } | null>(null)
const envEdits = reactive<Record<string, string | null>>({})
const newKey = ref('')
const newVal = ref('')
async function loadEnv() { try { envData.value = await api.get(`/api/projects/${encodeURIComponent(id.value)}/env`) } catch (e) { err.value = errorMessage(e) } }
watch(tab, (t) => { if (t === 'env' && !envData.value) loadEnv(); if (t === 'logs') loadLogs() })
function addVar() { if (newKey.value) { envEdits[newKey.value.toUpperCase()] = newVal.value; newKey.value = ''; newVal.value = '' } }
const envChanges = computed(() => ({ ...envEdits }))

// logs
const logs = ref<{ files: string[]; file: string | null; text: string } | null>(null)
const logFile = ref<string | null>(null)
async function loadLogs() {
  const q = new URLSearchParams({ lines: '300' })
  if (logFile.value) q.set('file', logFile.value)
  try { logs.value = await api.get(`/api/projects/${encodeURIComponent(id.value)}/logs?${q}`) } catch (e) { err.value = errorMessage(e) }
}
</script>

<template>
  <PageHeader :title="id" crumb="Deployments">
    <NuxtLink :to="`/sites?q=${encodeURIComponent(p.data.value?.site_id ?? id)}`" class="btn btn-s">Site</NuxtLink>
    <button type="button" class="btn btn-p" :disabled="!p.data.value" @click="deploy()"><AppIcon name="play" :size="14" :stroke="2" /> Deploy now</button>
  </PageHeader>
  <p v-if="err" class="small" style="color: var(--bad)" role="alert">{{ err }}</p>

  <div v-if="p.data.value" class="row wrap" style="gap: 8px">
    <span class="tag mono">{{ p.data.value.repo }}</span><span class="tag mono">{{ p.data.value.branch }}</span>
    <span class="tag">{{ p.data.value.kind }}</span><span class="tag mono">{{ p.data.value.dir }}</span><span class="tag">runs as {{ p.data.value.run_as }}</span>
    <StatusPill :tone="p.data.value.auto_deploy ? 'ok' : 'off'" :label="p.data.value.auto_deploy ? 'Deploys on push' : 'Manual deploys'" />
    <StatusPill v-if="status.data.value?.maintenance" tone="warn" label="Maintenance mode" />
  </div>

  <nav class="tabs" aria-label="Project sections">
    <button v-for="t in ([['deploys','Deploys'],['setup','GitHub setup'],['settings','Pipeline'],...(isLaravel ? [['app','Laravel']] : []),['env','.env'],['logs','Logs']] as const)" :key="t[0]" type="button" :class="{ on: tab === t[0] }" @click="tab = t[0] as any">{{ t[1] }}</button>
  </nav>

  <!-- deploys -->
  <template v-if="tab === 'deploys' && p.data.value">
    <section v-if="deployJob" class="card card-pad stack" style="gap: 8px">
      <strong>Deploying…</strong>
      <JobLog :job-id="deployJob" status="running" @finished="onDeployDone" />
    </section>
    <section class="card" style="overflow: hidden">
      <div v-if="!p.data.value.deploys.length" class="empty"><strong>No deploys yet</strong><span>Click “Deploy now” — the first deploy checks out the repository into {{ p.data.value.dir }}.</span></div>
      <div v-else class="table-wrap"><table>
        <thead><tr><th>Status</th><th>Commit</th><th>Trigger</th><th>When</th><th /></tr></thead>
        <tbody>
          <tr v-for="d in p.data.value.deploys" :key="d.id">
            <td><StatusPill :tone="tone(d.status)" :label="label(d.status)" /></td>
            <td style="white-space: normal"><span class="mono">{{ d.commit?.slice(0, 8) ?? '—' }}</span> {{ d.message }}<div v-if="d.author" class="small muted">{{ d.author }}</div><div v-if="d.error" class="small" style="color: var(--warn)">{{ d.error }}</div></td>
            <td class="small">{{ d.trigger }}</td>
            <td class="small">{{ formatRelative(d.started_at) }}</td>
            <td><div class="row" style="gap: 6px; justify-content: flex-end">
              <NuxtLink v-if="d.job_id" :to="`/activity?open=${d.job_id}`" class="btn btn-s">Log</NuxtLink>
              <ConfirmAction v-if="d.status === 'ok' && d.commit && d.commit !== p.data.value.last_commit" label="Roll back to this" :question="`Deploy ${d.commit.slice(0, 8)} again?`" confirm-label="Deploy it" @confirm="deploy(d.commit!)" />
            </div></td>
          </tr>
        </tbody>
      </table></div>
    </section>
  </template>

  <!-- setup -->
  <template v-if="tab === 'setup' && p.data.value">
    <section v-if="p.data.value.needs_key" class="card card-pad stack" style="gap: 10px">
      <h2>1. Add the deploy key to GitHub</h2>
      <p class="small text-2">GitHub → your repository → <strong>Settings → Deploy keys → Add deploy key</strong>. Paste this key, leave “Allow write access” off.</p>
      <div class="code-row"><code class="mono">{{ p.data.value.deploy_key }}</code><button type="button" class="btn btn-s" @click="copy(p.data.value.deploy_key ?? '')">Copy</button></div>
    </section>
    <section class="card card-pad stack" style="gap: 10px">
      <h2>{{ p.data.value.needs_key ? '2. ' : '' }}Deploy on push</h2>
      <p class="small text-2"><strong>Automatic (already works):</strong> with “Deploy on push” on, NexaCloud checks GitHub every minute and deploys new commits.</p>
      <p class="small text-2"><strong>Instant (webhook):</strong> GitHub → Settings → Webhooks → Add webhook. Content type <span class="mono">application/json</span>, “Just the push event”. GitHub needs a valid HTTPS certificate, so give the panel its own domain first.</p>
      <div class="kv"><span class="lbl">Payload URL</span><div class="code-row"><code class="mono">{{ panelOrigin }}{{ p.data.value.webhook_path }}</code><button type="button" class="btn btn-s" @click="copy(panelOrigin + p.data.value.webhook_path)">Copy</button></div></div>
      <div class="kv"><span class="lbl">Secret</span><div class="code-row"><code class="mono">{{ p.data.value.webhook_secret }}</code><button type="button" class="btn btn-s" @click="copy(p.data.value.webhook_secret)">Copy</button></div></div>
    </section>
    <section class="card card-pad stack" style="gap: 10px">
      <h2>Optional: test on GitHub Actions first</h2>
      <p class="small text-2">Add this file as <span class="mono">.github/workflows/deploy.yml</span> and the repository secrets <span class="mono">NEXACLOUD_URL</span> and <span class="mono">NEXACLOUD_SECRET</span>. GitHub runs your tests, and only a green build triggers the deploy. (Then turn off “Deploy on push” here.)</p>
      <pre class="yaml mono">{{ p.data.value.actions_yaml }}</pre>
      <button type="button" class="btn btn-s" style="align-self: flex-start" @click="copy(p.data.value.actions_yaml)">Copy workflow</button>
    </section>
  </template>

  <!-- pipeline settings -->
  <section v-if="tab === 'settings' && p.data.value" class="card card-pad stack" style="gap: 12px">
    <div class="fgrid">
      <label class="field"><span>Repository</span><input v-model="settings.repo" class="in mono" type="text"></label>
      <label class="field"><span>Branch</span><input v-model="settings.branch" class="in mono" type="text"></label>
    </div>
    <label class="small"><input v-model="settings.auto_deploy" type="checkbox"> Deploy automatically when I push</label>
    <label v-if="isLaravel" class="small"><input v-model="settings.migrate" type="checkbox"> Back up the database, then run migrations</label>
    <label class="field"><span>Deploy script (runs as {{ p.data.value.run_as }} in {{ p.data.value.dir }}, after git fetch/checkout)</span>
      <textarea v-model="settings.script" class="in mono script" rows="10" spellcheck="false" />
    </label>
    <p class="small muted">After the script NexaCloud reloads PHP-FPM and checks the site. If it returns errors (5xx), the previous commit is restored automatically.</p>
    <div class="row" style="gap: 10px"><button type="button" class="btn btn-p" @click="save">Save</button><span v-if="saved" class="small">{{ saved }}</span>
      <ConfirmAction label="Disconnect" question="Disconnect this site from Git? The site and its files stay." confirm-label="Disconnect" @confirm="disconnect" /></div>
  </section>

  <!-- laravel ops -->
  <template v-if="tab === 'app' && isLaravel && status.data.value">
    <section class="card card-pad stack" style="gap: 10px">
      <h2>Artisan</h2>
      <div class="row wrap" style="gap: 6px">
        <button v-for="c in ['optimize', 'optimize:clear', 'migrate', 'queue:restart', 'storage:link']" :key="c" type="button" class="btn btn-s mono" @click="runArtisan(c)">{{ c }}</button>
        <button type="button" class="btn btn-s" @click="runArtisan(status.data.value.maintenance ? 'up' : 'down')">{{ status.data.value.maintenance ? 'Leave maintenance mode' : 'Maintenance mode' }}</button>
      </div>
      <div class="row wrap" style="gap: 6px">
        <span class="mono small" style="color: var(--accent)">php artisan</span>
        <select v-model="artisanCmd" class="in mono" style="width: auto"><option v-for="c in status.data.value.artisan" :key="c">{{ c }}</option></select>
        <button type="button" class="btn btn-s btn-p" @click="runArtisan()">Run</button>
      </div>
      <JobLog v-if="artisanJob" :key="artisanJob" :job-id="artisanJob" status="running" @finished="status.refresh()" />
    </section>
    <section class="card card-pad stack" style="gap: 10px">
      <h2>Queue workers <StatusPill :tone="status.data.value.worker ? 'ok' : 'off'" :label="status.data.value.worker ? 'Configured' : 'Off'" style="margin-left: 8px" /></h2>
      <pre v-if="status.data.value.worker_status" class="yaml mono">{{ status.data.value.worker_status }}</pre>
      <div class="row wrap" style="gap: 8px">
        <label class="small">Processes <input v-model.number="workerForm.processes" class="in" type="number" min="1" max="8" style="width: 70px"></label>
        <label class="small">Queues <input v-model="workerForm.queue" class="in mono" type="text" style="width: 160px"></label>
      </div>
      <PlanAction :key="JSON.stringify(workerForm)" :request="{ kind: 'project_worker', params: { id, enabled: true, ...workerForm } }" :label="status.data.value.worker ? 'Update workers' : 'Start workers'" success-text="Workers are running under Supervisor." @done="status.refresh()" />
      <PlanAction v-if="status.data.value.worker" :request="{ kind: 'project_worker', params: { id, enabled: false } }" label="Stop and remove workers" success-text="Workers removed." @done="status.refresh()" />
    </section>
    <section class="card card-pad stack" style="gap: 10px">
      <h2>Scheduler <StatusPill :tone="status.data.value.scheduler ? 'ok' : 'off'" :label="status.data.value.scheduler ? 'Runs every minute' : 'Off'" style="margin-left: 8px" /></h2>
      <p class="small muted">Runs <span class="mono">php artisan schedule:run</span> every minute as {{ p.data.value?.run_as }}.</p>
      <PlanAction :key="String(status.data.value.scheduler)" :request="{ kind: 'project_scheduler', params: { id, enabled: !status.data.value.scheduler } }" :label="status.data.value.scheduler ? 'Turn off' : 'Turn on'" success-text="Done." @done="status.refresh()" />
    </section>
  </template>

  <!-- env -->
  <section v-if="tab === 'env'" class="card card-pad stack" style="gap: 10px">
    <p class="small muted">Secret values (passwords, keys, tokens) are hidden — type a new value to replace one. Changes go through Preview → Apply and can be rolled back.</p>
    <div v-for="v in envData?.vars ?? []" :key="v.key" class="env-row">
      <span class="mono small">{{ v.key }}</span>
      <input class="in mono" type="text" :placeholder="v.secret ? '•••••• (hidden)' : ''" :value="envEdits[v.key] ?? (v.secret ? '' : v.value ?? '')" @input="envEdits[v.key] = ($event.target as HTMLInputElement).value">
    </div>
    <div class="env-row"><input v-model="newKey" class="in mono" type="text" placeholder="NEW_KEY"><div class="row" style="gap: 6px"><input v-model="newVal" class="in mono" type="text" placeholder="value"><button type="button" class="btn btn-s" @click="addVar">Add</button></div></div>
    <PlanAction :key="JSON.stringify(envChanges)" :request="{ kind: 'project_env', params: { id, set: envChanges } }" label="Preview changes" :disabled="!Object.keys(envChanges).length" success-text=".env saved." @done="(s) => { if (s === 'applied') { for (const k of Object.keys(envEdits)) delete envEdits[k]; loadEnv() } }" />
  </section>

  <!-- logs -->
  <section v-if="tab === 'logs'" class="card card-pad stack" style="gap: 10px">
    <div class="row wrap" style="gap: 8px">
      <select v-model="logFile" class="in mono" style="width: auto" @change="loadLogs"><option :value="null">newest log</option><option v-for="f in logs?.files ?? []" :key="f" :value="f">{{ f }}</option></select>
      <button type="button" class="btn btn-s" @click="loadLogs">Refresh</button>
    </div>
    <pre class="yaml mono logs">{{ logs?.text || 'No log lines (storage/logs is empty).' }}</pre>
  </section>
</template>

<style scoped>
.tabs { display: flex; flex-wrap: wrap; gap: 4px; border-bottom: 1px solid #222A34; }
.tabs button { min-height: 44px; padding: 0 16px; background: transparent; border: 0; border-bottom: 2px solid transparent; color: var(--text-2); font-weight: 600; cursor: pointer; margin-bottom: -1px; }
.tabs button.on { color: var(--text); border-bottom-color: var(--accent); }
.fgrid { display: grid; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); gap: 12px; }
.field { display: flex; flex-direction: column; gap: 6px; }
.field > span:first-child { font-size: 13px; font-weight: 500; color: var(--text-2); }
.in { min-height: 38px; padding: 0 10px; background: var(--sunken, #0F1318); border: 1px solid var(--btn-line, #2A3441); border-radius: 8px; color: var(--text); font-size: 13px; width: 100%; box-sizing: border-box; }
.script { padding: 10px; min-height: 180px; line-height: 1.6; }
input[type=checkbox] { accent-color: var(--accent); }
.code-row { display: flex; gap: 8px; align-items: center; background: #07090C; border: 1px solid #1B222B; border-radius: 10px; padding: 8px 10px; }
.code-row code { flex: 1; font-size: 12px; overflow-wrap: anywhere; color: var(--text-2); }
.kv { display: flex; flex-direction: column; gap: 4px; }
.yaml { margin: 0; background: #07090C; border: 1px solid #1B222B; border-radius: 10px; padding: 12px 14px; font-size: 12px; line-height: 1.6; color: var(--text-2); overflow-x: auto; white-space: pre; }
.logs { max-height: 60vh; overflow: auto; white-space: pre-wrap; }
.env-row { display: grid; grid-template-columns: minmax(160px, 260px) 1fr; gap: 10px; align-items: center; }
@media (max-width: 640px) { .env-row { grid-template-columns: 1fr; } }
</style>
