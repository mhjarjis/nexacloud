<script setup lang="ts">
useHead({ title: 'New app · NexaCloud' })
const api = useApi()

const defaults = useResource<{ php_versions: string[]; mysql: boolean; postgres: boolean }>('/api/apps/defaults')
const step = ref(1)
const f = reactive({
  kind: 'laravel', domain: '', aliasWww: false, repo: '', branch: 'main', database: 'mysql',
  php_version: '', isolate: true, releases: true, https: true, ssl_email: '', worker: true, scheduler: true, auto_deploy: true,
})
watch(() => defaults.data.value, (d) => {
  if (!d) return
  if (!f.php_version) f.php_version = d.php_versions[d.php_versions.length - 1] ?? ''
  if (!d.mysql) f.database = d.postgres ? 'postgres' : 'none'
}, { immediate: true })

const domainOk = computed(() => /^(?=.{3,253}$)([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}$/.test(f.domain.trim().toLowerCase()))
const repoOk = computed(() => /^(git@[\w.-]+:[\w./-]+|https:\/\/[\w.-]+\/[\w./-]+|\/srv\/git\/[\w./-]+)$/.test(f.repo.trim()))
const dbName = computed(() => {
  let s = f.domain.trim().toLowerCase().replace(/[^a-z0-9]/g, '_')
  if (/^\d/.test(s)) s = 'db_' + s
  return s.length > 32 ? s.slice(0, 24) + '_…' : s
})
const canNext = computed(() => {
  if (step.value === 1) return domainOk.value
  if (step.value === 2) return repoOk.value && !!f.branch.trim()
  if (step.value === 3) return !!f.php_version && (!f.https || /.+@.+\..+/.test(f.ssl_email))
  return true
})
const STEPS = ['App', 'Code', 'Server', 'Review']

const err = ref<string | null>(null)
const job = ref<string | null>(null)
const result = ref<string | null>(null)
const busy = ref(false)
async function create() {
  err.value = null
  busy.value = true
  try {
    const body = {
      kind: f.kind, domain: f.domain.trim().toLowerCase(), aliases: f.aliasWww ? [`www.${f.domain.trim().toLowerCase()}`] : [],
      php_version: f.php_version, repo: f.repo.trim(), branch: f.branch.trim(), database: f.database,
      isolate: f.isolate, releases: f.releases, ssl_email: f.https ? f.ssl_email.trim() : '',
      worker: f.kind === 'laravel' && f.worker, scheduler: f.kind === 'laravel' && f.scheduler, auto_deploy: f.auto_deploy,
    }
    const r = await api.post<{ job_id: string; project_id: string }>('/api/apps', body)
    job.value = r.job_id
  } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) } finally { busy.value = false }
}
function onDone(s: string) { result.value = s }
const projectUrl = computed(() => `/projects/${encodeURIComponent(f.domain.trim().toLowerCase())}`)
const needsKey = computed(() => f.repo.trim().startsWith('git@'))
</script>

<template>
  <PageHeader title="New app" crumb="Domain → code → database → HTTPS → live, in one go" />

  <ol class="stepper" aria-label="Steps">
    <li v-for="(s, i) in STEPS" :key="s" :class="{ on: step === i + 1, done: step > i + 1 }" :aria-current="step === i + 1 ? 'step' : undefined">
      <span class="num">{{ step > i + 1 ? '✓' : i + 1 }}</span>{{ s }}
    </li>
  </ol>

  <section v-if="!job" class="card card-pad stack" style="gap: 16px; max-width: 860px">
    <!-- 1 -->
    <template v-if="step === 1">
      <h2>What are you deploying?</h2>
      <div class="choices" role="radiogroup" aria-label="App type">
        <label class="choice" :class="{ on: f.kind === 'laravel' }"><input v-model="f.kind" type="radio" value="laravel"> <strong>Laravel</strong><span class="small muted">migrations, queue worker, scheduler, artisan</span></label>
        <label class="choice" :class="{ on: f.kind === 'php' }"><input v-model="f.kind" type="radio" value="php"> <strong>Plain PHP</strong><span class="small muted">any PHP app with a public/ folder</span></label>
      </div>
      <label class="field"><span>Domain</span><input v-model="f.domain" class="in mono" type="text" placeholder="crm.nexadev.io" spellcheck="false" autocomplete="off"></label>
      <label class="tog"><input v-model="f.aliasWww" type="checkbox"> Also answer on www.{{ f.domain || 'domain' }}</label>
      <p class="small muted">Point the domain's DNS (A record) at this server before turning on HTTPS. Without it the app still works on the server; HTTPS can be added later.</p>
    </template>

    <!-- 2 -->
    <template v-if="step === 2">
      <h2>Where is the code?</h2>
      <div class="fgrid">
        <label class="field"><span>Repository</span><input v-model="f.repo" class="in mono" type="text" placeholder="git@github.com:nexadev/crm.git" spellcheck="false"></label>
        <label class="field"><span>Branch</span><input v-model="f.branch" class="in mono" type="text"></label>
      </div>
      <p class="small muted">Private GitHub repos: use the <span class="mono">git@github.com:…</span> form. NexaCloud makes a read-only deploy key and shows it after creating the app; the first deploy starts once you've added it on GitHub. Public repos (<span class="mono">https://…</span>) deploy right away.</p>
      <label class="tog"><input v-model="f.auto_deploy" type="checkbox"> Deploy automatically when I push to {{ f.branch || 'the branch' }}</label>
    </template>

    <!-- 3 -->
    <template v-if="step === 3">
      <h2>Server setup</h2>
      <div class="fgrid">
        <label class="field"><span>PHP version</span>
          <select v-model="f.php_version" class="in"><option v-for="v in defaults.data.value?.php_versions ?? []" :key="v" :value="v">PHP {{ v }}</option></select>
        </label>
        <label class="field"><span>Database</span>
          <select v-model="f.database" class="in">
            <option value="mysql" :disabled="!defaults.data.value?.mysql">MySQL{{ defaults.data.value?.mysql ? '' : ' (not installed)' }}</option>
            <option value="postgres" :disabled="!defaults.data.value?.postgres">PostgreSQL{{ defaults.data.value?.postgres ? '' : ' (not installed)' }}</option>
            <option value="none">None{{ f.kind === 'laravel' ? ' (SQLite file)' : '' }}</option>
          </select>
        </label>
      </div>
      <p v-if="!(defaults.data.value?.php_versions ?? []).length" class="note">No PHP-FPM version is installed yet. Install one on the <NuxtLink to="/php">PHP page</NuxtLink> first.</p>
      <div class="toggles">
        <label class="tog2"><span class="grow"><strong>Own Linux user and PHP pool</strong><span class="small muted">Another hacked site on this server can't read this app's .env or files.</span></span><input v-model="f.isolate" type="checkbox"></label>
        <label class="tog2"><span class="grow"><strong>Zero-downtime deploys</strong><span class="small muted">Each deploy builds in a new release folder and goes live only when it works. One-click rollback.</span></span><input v-model="f.releases" type="checkbox"></label>
        <label v-if="f.kind === 'laravel'" class="tog2"><span class="grow"><strong>Queue worker</strong><span class="small muted">One worker on the default queue under Supervisor, started after the first deploy.</span></span><input v-model="f.worker" type="checkbox"></label>
        <label v-if="f.kind === 'laravel'" class="tog2"><span class="grow"><strong>Scheduler</strong><span class="small muted">php artisan schedule:run every minute.</span></span><input v-model="f.scheduler" type="checkbox"></label>
        <label class="tog2"><span class="grow"><strong>HTTPS (Let's Encrypt)</strong><span class="small muted">Free certificate, renewed automatically. Needs the DNS to point here.</span></span><input v-model="f.https" type="checkbox"></label>
      </div>
      <label v-if="f.https" class="field"><span>Email for certificate notices</span><input v-model="f.ssl_email" class="in" type="email" placeholder="you@example.com"></label>
    </template>

    <!-- 4 -->
    <template v-if="step === 4">
      <h2>Ready to create</h2>
      <ul class="review">
        <li><span>Site</span><strong class="mono">{{ f.domain }}</strong><span class="muted">{{ f.kind === 'laravel' ? 'Laravel' : 'Plain PHP' }} · PHP {{ f.php_version }}{{ f.aliasWww ? ` · also www` : '' }}</span></li>
        <li><span>Runs as</span><strong>{{ f.isolate ? 'its own user + PHP pool' : 'www-data (shared)' }}</strong></li>
        <li><span>Code</span><strong class="mono">{{ f.repo }} @ {{ f.branch }}</strong><span class="muted">{{ f.releases ? 'zero-downtime releases' : 'in place' }}{{ f.auto_deploy ? ' · deploys on push' : '' }}</span></li>
        <li><span>Database</span><strong class="mono">{{ f.database === 'none' ? 'none' : `${f.database} · ${dbName}` }}</strong><span v-if="f.database !== 'none'" class="muted">own user, generated password written to .env</span></li>
        <li v-if="f.kind === 'laravel'"><span>.env</span><strong>production</strong><span class="muted">APP_KEY generated, debug off, daily logs</span></li>
        <li><span>HTTPS</span><strong>{{ f.https ? `Let's Encrypt (${f.ssl_email})` : 'later' }}</strong></li>
        <li v-if="f.kind === 'laravel'"><span>After deploy</span><strong>{{ [f.worker && 'queue worker', f.scheduler && 'scheduler'].filter(Boolean).join(' + ') || 'nothing' }}</strong></li>
      </ul>
      <p class="small muted">Every step goes through the same checks as manual changes (nginx test, site probe, automatic undo) and appears in Activity.</p>
      <p v-if="err" class="small" style="color: var(--bad)" role="alert">{{ err }}</p>
    </template>

    <div class="row wrap" style="gap: 10px; justify-content: space-between">
      <button v-if="step > 1" type="button" class="btn" @click="step--">Back</button><span v-else />
      <button v-if="step < 4" type="button" class="btn btn-p" :disabled="!canNext" @click="step++">Continue</button>
      <button v-else type="button" class="btn btn-p" :disabled="busy" @click="create">{{ busy ? 'Starting…' : 'Create app' }}</button>
    </div>
  </section>

  <section v-else class="card card-pad stack" style="gap: 12px; max-width: 980px">
    <h2>Creating {{ f.domain }}</h2>
    <JobLog :job-id="job" status="running" @finished="onDone" />
    <template v-if="result === 'ok'">
      <p v-if="needsKey" class="note">Almost there: open the project, copy its deploy key into GitHub (repository → Settings → Deploy keys), then press <strong>Deploy now</strong>.</p>
      <p v-else class="small" style="color: var(--ok)">The first deploy is running. Follow it on the project page.</p>
      <NuxtLink class="btn btn-p" :to="needsKey ? `${projectUrl}?tab=setup` : `${projectUrl}?tab=deploys`" style="align-self: flex-start">Open the project</NuxtLink>
    </template>
    <p v-else-if="result" class="small" style="color: var(--bad)">Something failed (see the log). Steps that succeeded stay in place; fix the problem and finish from the project or Sites page, or undo them from Activity.</p>
  </section>
</template>

<style scoped>
h2 { font-size: 18px; font-weight: 600; margin: 0; }
.stepper { list-style: none; margin: 0; padding: 0; display: flex; flex-wrap: wrap; gap: 8px; }
.stepper li { display: flex; align-items: center; gap: 8px; padding: 8px 14px 8px 8px; border-radius: 999px; background: var(--card); border: 1px solid var(--card-line); color: var(--muted); font-size: 14px; font-weight: 600; }
.stepper li.on { color: var(--text); border-color: var(--accent); }
.stepper li.done { color: var(--text-2); }
.num { width: 24px; height: 24px; border-radius: 50%; display: grid; place-items: center; background: #1B222B; font-size: 12px; }
.stepper li.on .num { background: var(--accent); color: var(--accent-ink); }
.choices { display: grid; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); gap: 12px; }
.choice { display: flex; flex-direction: column; gap: 4px; padding: 14px 16px; border: 1px solid var(--card-line); border-radius: 10px; cursor: pointer; }
.choice.on { border-color: var(--accent); background: rgba(61, 220, 151, .05); }
.choice input { position: absolute; opacity: 0; pointer-events: none; }
.choice:focus-within { outline: 2px solid var(--accent); outline-offset: 2px; }
.fgrid { display: grid; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); gap: 12px; }
.field { display: flex; flex-direction: column; gap: 6px; }
.field > span:first-child { font-size: 13px; font-weight: 500; color: var(--text-2); }
.in { min-height: 44px; padding: 0 12px; background: var(--sunken, #0F1318); border: 1px solid var(--btn-line, #2A3441); border-radius: 8px; color: var(--text); font-size: 14px; width: 100%; box-sizing: border-box; }
input[type=checkbox] { accent-color: var(--accent); width: 20px; height: 20px; flex: none; }
.tog { display: flex; align-items: center; gap: 10px; font-size: 14px; }
.toggles { display: flex; flex-direction: column; }
.tog2 { display: flex; align-items: center; gap: 12px; padding: 10px 0; border-bottom: 1px solid #1B222B; cursor: pointer; }
.tog2 .grow { display: flex; flex-direction: column; gap: 2px; font-size: 14px; }
.review { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; }
.review li { display: grid; grid-template-columns: 120px 1fr; gap: 2px 14px; padding: 10px 0; border-bottom: 1px solid #1B222B; font-size: 14px; }
.review li > span:first-child { color: var(--muted); grid-row: span 2; }
.review li .muted { font-size: 12px; grid-column: 2; }
</style>
