<script setup lang="ts">
// Board 02 · Projects + New project — same markup as the design, real data.
import type { PhpInfo, Site } from '~/types/api'

useHead({ title: 'Projects · NexaCloud' })
const api = useApi()
const route = useRoute()

interface Card {
  id: string
  site: Site
  project: { kind: string; repo: string; branch: string; strategy: string; auto_deploy: boolean; last_commit: string | null; dir: string } | null
  last_deploy: { status: string; started_at: string } | null
  monitor: { uptime: number | null; last_code: number | null; last_ms: number | null; p95: number | null }
  managed: boolean
}
const ws = useResource<Card[]>('/api/workspace', { interval: 30_000 })
const php = useResource<PhpInfo>('/api/php')
const defaults = useResource<{ php_versions: string[]; mysql: boolean; postgres: boolean }>('/api/apps/defaults')
const sys = useResource<{ hostname: string }>('/api/system')

// ---- list
const filter = ref<'all' | 'laravel' | 'php' | 'static'>('all')
const q = ref(String(route.query.q ?? ''))
const TINTS = ['#3DDC97', '#6CB4FF', '#F7C55E', '#B79CFF', '#FF9A9A', '#7FE0D4']
const all = computed(() => (ws.data.value ?? []).filter((c) => c.site.kind !== 'proxy').map((c, i) => {
  const code = c.monitor.last_code
  const failed = c.last_deploy && ['failed', 'rolled_back'].includes(c.last_deploy.status)
  const eol = php.data.value?.versions.find((v) => v.version === c.site.php_version)?.eol
  const [cls, status] = code !== null && (code === 0 || code >= 500) ? ['bad', 'Down']
    : failed ? ['bad', 'Deploy failed']
      : eol ? ['bad', 'PHP end-of-life']
        : c.site.ssl_expires && (daysUntil(c.site.ssl_expires) ?? 99) < 14 ? ['warn', 'SSL expiring']
          : code === null ? ['off', 'Not checked yet'] : ['ok', 'Healthy']
  return {
    id: c.id, k: c.site.kind,
    name: c.id,
    repo: c.project?.repo ? `${c.project.repo.replace(/^.*[:/]([^/:]+\/[^/]+?)(\.git)?$/, '$1')} · ${c.project.branch}` : (c.site.root ?? ''),
    fw: ({ laravel: 'Laravel', php: 'PHP (OOP)', static: 'Static' } as Record<string, string>)[c.site.kind] ?? c.site.kind,
    php: c.site.php_version,
    ssl: c.site.ssl ? 'HTTPS' : 'No HTTPS',
    extra: c.project?.repo ? (c.project.strategy === 'releases' ? 'Zero-downtime' : 'Git deploys') : (c.managed ? 'No Git yet' : 'Unmanaged'),
    status, cls,
    p95: c.monitor.p95 !== null ? `${c.monitor.p95} ms` : '—',
    uptime: c.monitor.uptime !== null ? `${(c.monitor.uptime * 100).toFixed(2)}%` : '—',
    disk: '—',
    tint: TINTS[i % TINTS.length]!,
    initial: c.id.replace(/^www\./, '').charAt(0).toUpperCase(),
    hasGit: !!c.project?.repo,
  }
}))
const shown = computed(() => all.value
  .filter((p) => filter.value === 'all' || p.k === filter.value)
  .filter((p) => !q.value.trim() || `${p.name} ${p.repo}`.toLowerCase().includes(q.value.trim().toLowerCase())))
const filters = computed(() => ([['all', 'All'], ['laravel', 'Laravel'], ['php', 'Plain PHP'], ['static', 'Static']] as const).map(([id, label]) => ({
  id, label: `${label} · ${all.value.filter((p) => id === 'all' || p.k === id).length}`,
  pressed: filter.value === id,
  bg: filter.value === id ? '#1F2833' : 'transparent',
  fg: filter.value === id ? '#E7ECF2' : '#A3AEBD',
})))
const deploying = ref<string | null>(null)
const listErr = ref<string | null>(null)
async function deploy(id: string) {
  deploying.value = id
  listErr.value = null
  try { await api.post(`/api/projects/${encodeURIComponent(id)}/deploy`, {}); await navigateTo(`/projects/${encodeURIComponent(id)}?tab=deploys`) } catch (e) { if (!(e instanceof SudoCancelled)) listErr.value = errorMessage(e) } finally { deploying.value = null }
}

// ---- new project
const kind = ref<'laravel' | 'php' | 'static'>('laravel')
const types = computed(() => ([['laravel', 'Laravel', 'Artisan, queues, scheduler'], ['php', 'Plain PHP', 'OOP / custom, any web root'], ['static', 'Static', 'HTML or built SPA']] as const).map(([id, label, hint]) => ({
  id, label, hint, pressed: kind.value === id,
  bg: kind.value === id ? '#13211B' : '#0F1318',
  border: kind.value === id ? '#3DDC97' : '#2A3441',
})))
const isLaravel = computed(() => kind.value === 'laravel')
const isPlain = computed(() => kind.value === 'php')
const f = reactive({ repo: '', branch: 'main', php: '', domain: '', webRoot: 'public_html', db: 'mysql', ssl: false, sslEmail: '', backup: true, worker: true, scheduler: true, npm: false, composer: true })
watch(() => defaults.data.value, (d) => { if (d && !f.php) f.php = d.php_versions[d.php_versions.length - 1] ?? '' }, { immediate: true })
const repoOptions = computed(() => [...new Set((ws.data.value ?? []).map((c) => c.project?.repo).filter((r): r is string => !!r))].map((r) => ({ value: r, label: r.replace(/^git@github\.com:/, '').replace(/\.git$/, ''), hint: 'used before' })))
const phpOptions = computed(() => {
  const v = defaults.data.value?.php_versions ?? []
  return v.map((x, i) => ({ value: x, label: i === v.length - 1 ? `${x} (recommended)` : x }))
})
const dbOptions = computed(() => [
  { id: 'mysql', label: 'MySQL', ok: defaults.data.value?.mysql !== false },
  { id: 'postgres', label: 'Postgres', ok: defaults.data.value?.postgres !== false },
  { id: 'none', label: 'None', ok: true },
])
const domainOk = computed(() => /^([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}$/.test(f.domain.trim().toLowerCase()))
const repoOk = computed(() => /^(git@[\w.-]+:[\w./-]+|https:\/\/[\w.-]+\/[\w./-]+|\/srv\/git\/[\w./-]+)$/.test(f.repo.trim()))
const host = computed(() => sys.data.value?.hostname ?? 'server')
const d = computed(() => f.domain.trim().toLowerCase() || 'blog.example.com')
const plan = computed(() => {
  const lines: string[] = [`✓ /var/www/${d.value} (own Linux user)`]
  if (kind.value === 'static') lines.push(`✓ nginx vhost with gzip + cache headers`)
  else lines.push(`✓ nginx vhost → /${isLaravel.value ? 'public' : (f.webRoot.trim().replace(/^\/+/, '') || '')} · PHP-FPM ${f.php} pool`)
  if (kind.value !== 'static' && f.db !== 'none') lines.push(`✓ ${f.db === 'mysql' ? 'MySQL' : 'PostgreSQL'} db + user${isLaravel.value ? ', written to .env' : ''}`)
  if (isLaravel.value) lines.push('✓ production .env (APP_KEY, debug off)')
  if (f.ssl) lines.push('✓ SSL certificate + HTTP→HTTPS redirect')
  if (isLaravel.value && (f.worker || f.scheduler)) lines.push(`✓ ${[f.worker && 'Supervisor worker', f.scheduler && 'cron scheduler'].filter(Boolean).join(' + ')}`)
  if (f.backup && f.db !== 'none' && kind.value !== 'static') lines.push('✓ nightly database backup')
  lines.push(`✓ ${f.repo.trim() ? 'first deploy from Git → zero-downtime releases' : 'Git repository needed'}`)
  return lines.join('\n')
})
const canCreate = computed(() => domainOk.value && repoOk.value && (kind.value === 'static' || !!f.php) && (!f.ssl || /.+@.+\..+/.test(f.sslEmail)))
const creating = ref(false)
const createErr = ref<string | null>(null)
const job = ref<string | null>(null)
const jobState = ref<string | null>(null)
const newId = ref('')
async function create() {
  createErr.value = null
  creating.value = true
  try {
    const body = {
      kind: kind.value, domain: f.domain.trim().toLowerCase(), php_version: f.php || '8.3', repo: f.repo.trim(), branch: f.branch.trim() || 'main',
      database: kind.value === 'static' ? 'none' : f.db, isolate: true, releases: true, ssl_email: f.ssl ? f.sslEmail.trim() : '',
      worker: isLaravel.value && f.worker, scheduler: isLaravel.value && f.scheduler, auto_deploy: true,
      web_root: isLaravel.value ? 'public' : kind.value === 'php' ? (f.webRoot.trim().replace(/^\/+|\/+$/g, '') || null) : null,
      backup: f.backup && kind.value !== 'static' && f.db !== 'none', npm_build: isLaravel.value ? f.npm : true, composer: kind.value === 'static' ? false : (isPlain.value ? f.composer : true),
    }
    const r = await api.post<{ job_id: string; project_id: string }>('/api/apps', body)
    job.value = r.job_id
    newId.value = r.project_id
  } catch (e) { if (!(e instanceof SudoCancelled)) createErr.value = errorMessage(e) } finally { creating.value = false }
}
function onJobDone(s: string) { jobState.value = s; ws.refresh() }
function resetForm() { job.value = null; jobState.value = null; f.domain = ''; f.repo = '' }
onMounted(() => { if (route.query.new) setTimeout(() => document.getElementById('newp')?.scrollIntoView({ behavior: 'smooth' }), 200) })
</script>

<template>
  <header style="display: flex; flex-wrap: wrap; align-items: flex-end; justify-content: space-between; gap: 16px">
    <div style="display: flex; flex-direction: column; gap: 6px">
      <p class="lbl">{{ host }} / Projects</p>
      <h1 style="font-size: 30px; font-weight: 600">Projects</h1>
    </div>
    <div style="display: flex; flex-wrap: wrap; gap: 10px; align-items: center">
      <label style="display: flex; align-items: center; gap: 8px; min-height: 44px; padding: 0 12px; background: #12171D; border: 1px solid #222A34; border-radius: 8px; min-width: 240px; box-sizing: border-box">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="#A3AEBD" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="11" cy="11" r="7" /><path d="M20 20l-3.5-3.5" /></svg>
        <input v-model="q" type="search" aria-label="Search projects" placeholder="Search projects or repositories" style="flex: 1; background: transparent; border: 0; outline: none; color: #E7ECF2; font-size: 14px; min-width: 0">
      </label>
      <div role="group" aria-label="Filter by type" style="display: flex; flex-wrap: wrap; gap: 6px; padding: 4px; background: #12171D; border: 1px solid #222A34; border-radius: 10px">
        <button v-for="fl in filters" :key="fl.id" type="button" :aria-pressed="fl.pressed" :style="`min-height: 36px; padding: 0 14px; border-radius: 7px; border: 0; cursor: pointer; font-size: 13px; font-weight: 600; background: ${fl.bg}; color: ${fl.fg}`" @click="filter = fl.id">{{ fl.label }}</button>
      </div>
    </div>
  </header>
  <p v-if="listErr" class="small" style="color: var(--bad)" role="alert">{{ listErr }}</p>

  <div style="display: flex; flex-wrap: wrap; gap: 20px; align-items: flex-start">
    <section aria-label="Project list" style="flex: 3 1 560px; min-width: 0; display: grid; grid-template-columns: repeat(auto-fill, minmax(300px, 1fr)); gap: 16px">
      <article v-for="p in shown" :key="p.id" class="card" style="padding: 18px; display: flex; flex-direction: column; gap: 14px">
        <div style="display: flex; align-items: flex-start; gap: 12px">
          <div :style="`width: 40px; height: 40px; border-radius: 10px; background: ${p.tint}; display: grid; place-items: center; flex: none; font-family: 'Space Grotesk', sans-serif; font-weight: 700; color: #0B0E12`" aria-hidden="true">{{ p.initial }}</div>
          <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 3px">
            <h2 style="font-size: 16px; font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap">{{ p.name }}</h2>
            <span class="mono" style="font-size: 12px; color: #A3AEBD; overflow: hidden; text-overflow: ellipsis; white-space: nowrap">{{ p.repo }}</span>
          </div>
          <span :class="`pill ${p.cls}`">{{ p.status }}</span>
        </div>
        <div style="display: flex; flex-wrap: wrap; gap: 6px">
          <span class="tag">{{ p.fw }}</span><span v-if="p.php" class="tag mono">PHP {{ p.php }}</span><span class="tag">{{ p.ssl }}</span><span class="tag">{{ p.extra }}</span>
        </div>
        <div style="display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 8px; padding: 12px; background: #0F1318; border-radius: 10px">
          <div style="display: flex; flex-direction: column; gap: 3px"><span class="lbl">p95</span><span class="mono" style="font-size: 14px">{{ p.p95 }}</span></div>
          <div style="display: flex; flex-direction: column; gap: 3px"><span class="lbl">Uptime</span><span class="mono" style="font-size: 14px">{{ p.uptime }}</span></div>
          <div style="display: flex; flex-direction: column; gap: 3px"><span class="lbl">Type</span><span class="mono" style="font-size: 14px">{{ p.k === 'laravel' ? 'Laravel' : p.k === 'php' ? 'PHP' : 'Static' }}</span></div>
        </div>
        <div style="display: flex; flex-wrap: wrap; gap: 8px">
          <NuxtLink class="btn btn-s" :to="`/projects/${encodeURIComponent(p.id)}`" style="flex: 1">Open</NuxtLink>
          <button v-if="p.hasGit" type="button" class="btn btn-s" :disabled="deploying === p.id" @click="deploy(p.id)">{{ deploying === p.id ? 'Starting…' : 'Deploy' }}</button>
          <NuxtLink v-else class="btn btn-s" :to="`/projects/${encodeURIComponent(p.id)}?tab=deploys`">Connect Git</NuxtLink>
          <NuxtLink class="btn btn-s" :to="`/projects/${encodeURIComponent(p.id)}?tab=logs`">Logs</NuxtLink>
        </div>
      </article>
      <div v-if="!shown.length && !ws.loading.value" class="card" style="padding: 28px; display: flex; flex-direction: column; gap: 8px; align-items: flex-start; grid-column: 1 / -1">
        <strong>{{ q || filter !== 'all' ? 'No projects match' : 'No projects yet' }}</strong>
        <span style="font-size: 13px; color: #A3AEBD">Create one with the form on the right, or <NuxtLink to="/install">import the sites already running on this server</NuxtLink>.</span>
      </div>
    </section>

    <section id="newp" class="card" aria-labelledby="newp-h" style="flex: 1 1 380px; min-width: 0; padding: 22px; display: flex; flex-direction: column; gap: 18px">
      <div style="display: flex; flex-direction: column; gap: 4px">
        <h2 id="newp-h" style="font-size: 19px; font-weight: 600">New project</h2>
        <p style="font-size: 13px; color: #A3AEBD; line-height: 1.5">Creates the Nginx site, PHP-FPM pool, database, SSL and a GitHub deploy hook in one go.</p>
      </div>

      <template v-if="!job">
        <fieldset style="border: 0; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 8px">
          <legend style="font-size: 13px; font-weight: 500; color: #C5CEDA; margin-bottom: 8px">1. What are you deploying?</legend>
          <div style="display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 8px">
            <button v-for="t in types" :key="t.id" type="button" :aria-pressed="t.pressed" :style="`min-height: 84px; padding: 12px 10px; border-radius: 10px; cursor: pointer; display: flex; flex-direction: column; align-items: flex-start; gap: 6px; text-align: left; background: ${t.bg}; border: 1px solid ${t.border}; color: #E7ECF2`" @click="kind = t.id">
              <span style="font-size: 14px; font-weight: 600">{{ t.label }}</span>
              <span style="font-size: 12px; color: #A3AEBD; line-height: 1.35">{{ t.hint }}</span>
            </button>
          </div>
        </fieldset>

        <div style="display: flex; flex-direction: column; gap: 12px">
          <p style="font-size: 13px; font-weight: 500; color: #C5CEDA">2. Source &amp; domain</p>
          <label class="field"><span>GitHub repository</span>
            <SearchSelect v-model="f.repo" :options="repoOptions" allow-custom mono placeholder="git@github.com:owner/repo.git" aria-label="GitHub repository" />
          </label>
          <div style="display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px">
            <label class="field"><span>Branch</span><SearchSelect v-model="f.branch" :options="['main', 'master', 'production', 'develop']" allow-custom mono placeholder="main" aria-label="Branch" /></label>
            <label v-if="kind !== 'static'" class="field"><span>PHP version</span><SearchSelect v-model="f.php" :options="phpOptions" placeholder="Choose…" aria-label="PHP version" /></label>
          </div>
          <label class="field"><span>Domain or subdomain</span><input v-model="f.domain" class="in" type="text" placeholder="blog.example.com" autocomplete="off" spellcheck="false"></label>
          <p v-if="isLaravel" style="font-size: 12px; color: #A3AEBD">Web root is set to <span class="mono" style="color: #C5CEDA">/public</span> automatically.</p>
          <label v-if="isPlain" class="field"><span>Web root (folder with index.php)</span><input v-model="f.webRoot" class="in mono" type="text" placeholder="/public_html"></label>
          <p style="font-size: 12px; color: #A3AEBD">Private repos: use the git@github.com:… form — the read-only deploy key is shown on the project page.</p>
        </div>

        <div v-if="kind !== 'static'" style="display: flex; flex-direction: column; gap: 12px">
          <p style="font-size: 13px; font-weight: 500; color: #C5CEDA">3. Database</p>
          <div style="display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 8px" role="radiogroup" aria-label="Database">
            <label v-for="o in dbOptions" :key="o.id" :style="`display: flex; align-items: center; gap: 8px; min-height: 44px; padding: 0 12px; border: 1px solid ${f.db === o.id ? '#3DDC97' : '#2A3441'}; border-radius: 8px; ${f.db === o.id ? 'background: #13211B;' : ''} font-size: 14px; cursor: pointer; ${o.ok ? '' : 'opacity: .45;'}`"><input v-model="f.db" type="radio" name="db" :value="o.id" :disabled="!o.ok" style="accent-color: #3DDC97">{{ o.label }}</label>
          </div>
        </div>

        <div style="display: flex; flex-direction: column; gap: 10px">
          <p style="font-size: 13px; font-weight: 500; color: #C5CEDA">{{ kind === 'static' ? '3' : '4' }}. Extras</p>
          <label style="display: flex; align-items: center; gap: 10px; font-size: 14px; min-height: 32px"><input v-model="f.ssl" type="checkbox" style="width: 18px; height: 18px; accent-color: #3DDC97">Free SSL (Let's Encrypt, auto-renew)</label>
          <input v-if="f.ssl" v-model="f.sslEmail" class="in" type="email" placeholder="Email for certificate notices (you@example.com)" aria-label="Email for certificate notices">
          <label v-if="kind !== 'static'" style="display: flex; align-items: center; gap: 10px; font-size: 14px; min-height: 32px"><input v-model="f.backup" type="checkbox" :disabled="f.db === 'none'" style="width: 18px; height: 18px; accent-color: #3DDC97">Nightly database backup</label>
          <div v-if="isLaravel" style="display: flex; flex-direction: column; gap: 10px">
            <label style="display: flex; align-items: center; gap: 10px; font-size: 14px; min-height: 32px"><input v-model="f.worker" type="checkbox" style="width: 18px; height: 18px; accent-color: #3DDC97">Queue worker (Supervisor, 2 processes)</label>
            <label style="display: flex; align-items: center; gap: 10px; font-size: 14px; min-height: 32px"><input v-model="f.scheduler" type="checkbox" style="width: 18px; height: 18px; accent-color: #3DDC97">Scheduler (php artisan schedule:run)</label>
            <label style="display: flex; align-items: center; gap: 10px; font-size: 14px; min-height: 32px"><input v-model="f.npm" type="checkbox" style="width: 18px; height: 18px; accent-color: #3DDC97">Build front-end assets (npm run build)</label>
          </div>
          <label v-if="isPlain" style="display: flex; align-items: center; gap: 10px; font-size: 14px; min-height: 32px"><input v-model="f.composer" type="checkbox" style="width: 18px; height: 18px; accent-color: #3DDC97">Run composer install on deploy</label>
        </div>

        <div style="padding: 12px 14px; background: #0F1318; border: 1px solid #222A34; border-radius: 10px; display: flex; flex-direction: column; gap: 6px">
          <span class="lbl">What will happen</span>
          <span class="mono" style="font-size: 12px; color: #C5CEDA; line-height: 1.7; white-space: pre-line">{{ plan }}</span>
        </div>
        <p v-if="createErr" class="small" style="color: var(--bad)" role="alert">{{ createErr }}</p>
        <button type="button" class="btn btn-p" style="width: 100%" :disabled="!canCreate || creating" @click="create">{{ creating ? 'Starting…' : 'Create project' }}</button>
      </template>

      <template v-else>
        <JobLog :job-id="job" status="running" @finished="onJobDone" />
        <template v-if="jobState === 'ok'">
          <p v-if="f.repo.trim().startsWith('git@')" class="note">Add the deploy key shown on the project page to GitHub, then press Deploy now.</p>
          <p v-else style="font-size: 13px; color: var(--ok)">Created. The first deploy is running.</p>
          <div style="display: flex; gap: 8px; flex-wrap: wrap"><NuxtLink class="btn btn-p" :to="`/projects/${encodeURIComponent(newId)}${f.repo.trim().startsWith('git@') ? '?tab=deploys' : '?tab=deploys'}`">Open project</NuxtLink><button type="button" class="btn" @click="resetForm">New project</button></div>
        </template>
        <div v-else-if="jobState" style="display: flex; flex-direction: column; gap: 8px">
          <p style="font-size: 13px; color: var(--bad)">Something failed — see the log. Finished steps stay in place (undo them under Activity).</p>
          <button type="button" class="btn" style="align-self: flex-start" @click="resetForm">Back to the form</button>
        </div>
      </template>
    </section>
  </div>
</template>

<style scoped>
.field { display: flex; flex-direction: column; gap: 6px; }
.field > span:first-child { font-size: 13px; font-weight: 500; color: #C5CEDA; }
.in { min-height: 44px; padding: 0 12px; width: 100%; box-sizing: border-box; }
</style>
