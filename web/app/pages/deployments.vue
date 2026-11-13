<script setup lang="ts">
// Board 06 · GitHub CI/CD — same markup as the design, real data.
useHead({ title: 'Deployments · NexaCloud' })
const api = useApi()
const route = useRoute()

interface Step { name: string; cmd: string; status: string; secs: number | null; result: string }
interface Run { id: string; project_id: string; job_id: string | null; status: string; trigger: string; commit: string | null; message: string | null; author: string | null; started_at: string; error: string | null; steps: Step[]; release: string | null }
interface Proj { id: string; site_id: string; kind: string; repo: string; branch: string; script: string; migrate: boolean; auto_deploy: boolean; run_tests: boolean; notify: boolean; strategy: string; keep_releases: number; actions_yaml: string; last_commit: string | null }

const runsRes = useResource<Run[]>('/api/deploys', { interval: 8_000 })
const projectsRes = useResource<Proj[]>('/api/projects', { interval: 30_000 })
const sys = useResource<{ hostname: string }>('/api/system')
const host = computed(() => sys.data.value?.hostname ?? 'server')
const gitProjects = computed(() => (projectsRes.data.value ?? []).filter((p) => p.repo))

const LABEL: Record<string, string> = { ok: 'Live', running: 'Deploying', queued: 'Queued', failed: 'Failed', rolled_back: 'Rolled back' }
const CLS: Record<string, string> = { ok: 'ok', running: 'info', queued: 'info', failed: 'bad', rolled_back: 'bad' }
const numbered = computed(() => {
  const all = runsRes.data.value ?? []
  const counts: Record<string, number> = {}
  for (const r of [...all].reverse()) counts[r.id] = (counts[`#${r.project_id}`] = (counts[`#${r.project_id}`] ?? 0) + 1)
  return all.map((r) => ({ ...r, n: counts[r.id] ?? 0 }))
})
const sel = ref<string | null>(null)
const selected = computed(() => numbered.value.find((r) => r.id === sel.value) ?? numbered.value[0] ?? null)
const runs = computed(() => numbered.value.slice(0, 12).map((r) => ({
  ...r, pressed: selected.value?.id === r.id,
  bg: selected.value?.id === r.id ? '#1B2430' : 'transparent',
  border: selected.value?.id === r.id ? '#2E3B4B' : 'transparent',
})))
const STEP_STYLE: Record<string, { bg: string; fg: string; icon: string }> = {
  ok: { bg: 'rgba(61,220,151,.14)', fg: '#5BE6A9', icon: '✓' },
  failed: { bg: 'rgba(255,122,122,.16)', fg: '#FF9A9A', icon: '✕' },
  running: { bg: 'rgba(108,180,255,.14)', fg: '#8CC4FF', icon: '…' },
  skipped: { bg: '#1B222B', fg: '#A3AEBD', icon: '–' },
}
const log = ref('')
watch(() => selected.value?.job_id, async (j) => {
  log.value = ''
  if (!j) return
  try { const job = await api.get<{ log: string }>(`/api/jobs/${j}`); log.value = job.log.split('\n').slice(-14).map((l) => l.replace(/^\[[^\]]+\]\s?/, '')).join('\n') } catch { log.value = '' }
}, { immediate: true })
const note = computed(() => {
  const r = selected.value
  if (!r) return ''
  if (r.status === 'ok') return r.release ? `Zero-downtime: release ${r.release} went live only after the health check passed.` : 'Live — the health check passed.'
  if (r.status === 'rolled_back') return r.error ?? 'The site failed its health check, so the previous version is live again.'
  if (r.status === 'failed') return r.steps.some((s) => s.name === 'Switch release' && s.status === 'ok') ? (r.error ?? '') : 'Nothing changed on the live site.'
  return 'Running…'
})
const busy = ref(false)
const msg = ref<string | null>(null)
async function deploy(projectId: string, commit?: string) {
  busy.value = true
  msg.value = null
  try { await api.post(`/api/projects/${encodeURIComponent(projectId)}/deploy`, commit ? { commit } : {}); sel.value = null; setTimeout(() => runsRes.refresh(), 800) } catch (e) { if (!(e instanceof SudoCancelled)) msg.value = errorMessage(e) } finally { busy.value = false }
}
const previousOk = computed(() => {
  const r = selected.value
  if (!r) return null
  return numbered.value.find((x) => x.project_id === r.project_id && x.status === 'ok' && x.commit && x.commit !== r.commit && new Date(x.started_at) < new Date(r.started_at)) ?? null
})

// "Deploy a project"
const deployPick = ref('')
const picking = ref(false)

// pipeline
const pipeId = ref(String(route.query.project ?? ''))
watch(gitProjects, (g) => { if (!pipeId.value && g.length) pipeId.value = selected.value?.project_id ?? g[0]!.id }, { immediate: true })
const pipe = computed(() => gitProjects.value.find((p) => p.id === pipeId.value) ?? null)
const form = reactive({ auto_deploy: true, run_tests: false, migrate: true, notify: true, script: '' })
watch(pipe, (p) => { if (p) Object.assign(form, { auto_deploy: p.auto_deploy, run_tests: p.run_tests, migrate: p.migrate, notify: p.notify, script: p.script }) }, { immediate: true })
const toggles = computed(() => [
  { key: 'strategy', name: 'Zero-downtime releases', hint: pipe.value?.strategy === 'releases' ? 'Build in releases/N, switch symlink when healthy' : 'Off — switch on from the project’s Deployments tab', on: pipe.value?.strategy === 'releases', readonly: true },
  { key: 'run_tests', name: 'Stop if tests fail', hint: 'Runs php artisan test before going live', on: form.run_tests, readonly: pipe.value?.kind !== 'laravel' },
  { key: 'migrate', name: 'Back up database before migrate', hint: 'Quick dump, kept 7 deploys', on: form.migrate, readonly: pipe.value?.kind !== 'laravel' },
  { key: 'notify', name: 'Notify on failure', hint: 'Telegram / email from Monitoring', on: form.notify, readonly: false },
])
function setToggle(k: string, v: boolean) { if (k in form) (form as Record<string, unknown>)[k] = v }
const saved = ref<string | null>(null)
async function savePipe() {
  if (!pipe.value) return
  saved.value = null
  try { await api.put(`/api/projects/${encodeURIComponent(pipe.value.id)}`, { ...form }); saved.value = 'Saved.'; projectsRes.refresh() } catch (e) { if (!(e instanceof SudoCancelled)) saved.value = errorMessage(e) }
}
const DEFAULT_LARAVEL = '# Runs as the site user inside the new release, after the code is checked out.\nif [ -f composer.json ] && command -v composer >/dev/null; then\n  composer install --no-dev --no-interaction --prefer-dist --optimize-autoloader\nfi\nif [ -f package.json ] && grep -q \'"build"\' package.json && command -v npm >/dev/null; then\n  npm ci --no-audit --no-fund && npm run build\nfi\nphp artisan migrate --force\nphp artisan optimize\nphp artisan queue:restart\n'
const releasesRes = ref<{ name: string; current: boolean; commit: string; message: string }[]>([])
watch(pipeId, async (id) => {
  releasesRes.value = []
  if (!id) return
  try { releasesRes.value = (await api.get<{ releases: typeof releasesRes.value }>(`/api/projects/${encodeURIComponent(id)}/releases`)).releases } catch { /* none */ }
}, { immediate: true })
const activating = ref<string | null>(null)
async function activate(name: string) {
  if (!pipe.value) return
  activating.value = name
  try { await api.post(`/api/projects/${encodeURIComponent(pipe.value.id)}/releases/${encodeURIComponent(name)}/activate`, {}); setTimeout(() => runsRes.refresh(), 800) } catch (e) { if (!(e instanceof SudoCancelled)) msg.value = errorMessage(e) } finally { activating.value = null }
}
async function copy(t: string) { await navigator.clipboard?.writeText(t) }
const shortRepo = (r: string) => r.replace(/^.*[:/]([^/:]+\/[^/]+?)(\.git)?$/, '$1')
</script>

<template>
  <header style="display: flex; flex-wrap: wrap; align-items: flex-end; justify-content: space-between; gap: 16px">
    <div style="display: flex; flex-direction: column; gap: 6px">
      <p class="lbl">{{ host }} / Deployments</p>
      <h1 style="font-size: 30px; font-weight: 600">GitHub CI/CD</h1>
    </div>
    <div style="display: flex; flex-wrap: wrap; align-items: center; gap: 10px">
      <span style="display: flex; align-items: center; gap: 8px; min-height: 44px; padding: 0 14px; border: 1px solid #222A34; border-radius: 8px; background: #12171D; font-size: 13px">
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="#E7ECF2" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 19c-4.3 1.4-4.3-2.5-6-3m12 5v-3.5c0-1 .1-1.4-.5-2 2.8-.3 5.5-1.4 5.5-6a4.6 4.6 0 00-1.3-3.2 4.2 4.2 0 00-.1-3.2s-1.1-.3-3.5 1.3a12.3 12.3 0 00-6.2 0C6.5 2.8 5.4 3.1 5.4 3.1a4.2 4.2 0 00-.1 3.2A4.6 4.6 0 004 9.5c0 4.6 2.7 5.7 5.5 6-.6.6-.6 1.2-.5 2V21" /></svg>
        Deploy keys · <strong>{{ gitProjects.length }}</strong> {{ gitProjects.length === 1 ? 'repo' : 'repos' }}
      </span>
      <div v-if="picking" style="display: flex; gap: 8px; align-items: center">
        <SearchSelect v-model="deployPick" :options="gitProjects.map((p) => ({ value: p.id, label: p.id, hint: shortRepo(p.repo) }))" placeholder="Choose a project…" aria-label="Project to deploy" style="width: 240px" />
        <button type="button" class="btn btn-p" :disabled="!deployPick || busy" @click="deploy(deployPick); picking = false">Deploy</button>
      </div>
      <button v-else type="button" class="btn btn-p" :disabled="!gitProjects.length" @click="picking = true">Deploy a project</button>
    </div>
  </header>
  <p v-if="msg" class="small" style="color: var(--bad)" role="alert">{{ msg }}</p>

  <div v-if="!numbered.length && !runsRes.loading.value" class="card" style="padding: 28px; display: flex; flex-direction: column; gap: 8px">
    <strong>No deployments yet</strong>
    <span style="font-size: 13px; color: #A3AEBD">Connect a project to a Git repository (Projects → open a project → Deployments), or create a <NuxtLink to="/projects?new=1">new project from GitHub</NuxtLink>.</span>
  </div>

  <div v-else style="display: flex; flex-wrap: wrap; gap: 16px; align-items: flex-start">
    <section class="card" aria-labelledby="runs" style="flex: 1 1 340px; min-width: 0; padding: 8px; display: flex; flex-direction: column; gap: 2px">
      <h2 id="runs" style="font-size: 17px; font-weight: 600; padding: 10px 12px 8px">Recent runs</h2>
      <button v-for="r in runs" :key="r.id" type="button" :aria-pressed="r.pressed" :style="`display: flex; align-items: center; gap: 12px; padding: 12px; min-height: 60px; border-radius: 9px; border: 1px solid ${r.border}; background: ${r.bg}; color: #E7ECF2; cursor: pointer; text-align: left`" @click="sel = r.id">
        <span :class="`pill ${CLS[r.status] ?? 'off'}`">{{ LABEL[r.status] ?? r.status }}</span>
        <span style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px">
          <span style="font-size: 14px; font-weight: 600">{{ r.project_id }} <span style="color: #A3AEBD; font-weight: 500">#{{ r.n }}</span></span>
          <span class="mono" style="font-size: 12px; color: #A3AEBD; overflow: hidden; text-overflow: ellipsis; white-space: nowrap">{{ r.commit?.slice(0, 7) ?? '—' }} · {{ r.message ?? r.trigger }}</span>
        </span>
        <span style="font-size: 12px; color: #A3AEBD; white-space: nowrap">{{ formatRelative(r.started_at) }}</span>
      </button>
    </section>

    <section v-if="selected" class="card" aria-labelledby="rd" style="flex: 2 1 560px; min-width: 0; padding: 20px; display: flex; flex-direction: column; gap: 16px">
      <div style="display: flex; flex-wrap: wrap; align-items: flex-start; justify-content: space-between; gap: 12px">
        <div style="display: flex; flex-direction: column; gap: 6px">
          <div style="display: flex; flex-wrap: wrap; align-items: center; gap: 10px"><h2 id="rd" style="font-size: 20px; font-weight: 600">{{ selected.project_id }} #{{ selected.n }}</h2><span :class="`pill ${CLS[selected.status] ?? 'off'}`">{{ LABEL[selected.status] ?? selected.status }}</span></div>
          <span class="mono" style="font-size: 12px; color: #A3AEBD">{{ gitProjects.find((p) => p.id === selected!.project_id)?.branch ?? '' }} · {{ selected.commit?.slice(0, 7) ?? '—' }} · {{ selected.message ?? '' }}<template v-if="selected.author"> · by {{ selected.author }}</template></span>
        </div>
        <div style="display: flex; gap: 8px; flex-wrap: wrap">
          <button type="button" class="btn btn-s" :disabled="busy" @click="deploy(selected.project_id)">Redeploy</button>
          <button type="button" class="btn btn-s" :disabled="busy || !previousOk" :title="previousOk ? `Deploy ${previousOk.commit?.slice(0, 7)} again` : 'No earlier good deploy'" @click="previousOk && deploy(selected.project_id, previousOk.commit!)">Roll back to previous</button>
        </div>
      </div>
      <ol style="list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 0">
        <li v-for="(s, i) in selected.steps" :key="i" style="display: flex; align-items: center; gap: 12px; padding: 10px 0; border-bottom: 1px solid #1B222B">
          <span :style="`width: 26px; height: 26px; border-radius: 50%; display: grid; place-items: center; font-size: 13px; font-weight: 700; flex: none; background: ${(STEP_STYLE[s.status] ?? STEP_STYLE.skipped)!.bg}; color: ${(STEP_STYLE[s.status] ?? STEP_STYLE.skipped)!.fg}`" aria-hidden="true">{{ (STEP_STYLE[s.status] ?? STEP_STYLE.skipped)!.icon }}</span>
          <span style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px"><span style="font-size: 14px; font-weight: 600">{{ s.name }}</span><span class="mono" style="font-size: 12px; color: #A3AEBD; overflow: hidden; text-overflow: ellipsis; white-space: nowrap">{{ s.cmd }}</span></span>
          <span style="font-size: 12px; color: #C5CEDA">{{ s.result || s.status }}</span>
          <span class="mono" style="font-size: 12px; color: #A3AEBD; width: 44px; text-align: right">{{ s.secs !== null ? `${Math.round(s.secs)}s` : '—' }}</span>
        </li>
        <li v-if="!selected.steps.length" style="font-size: 13px; color: #A3AEBD; padding: 10px 0">Step details are recorded for deploys made with NexaCloud 0.4+.</li>
      </ol>
      <div class="mono" style="background: #07090C; border: 1px solid #1B222B; border-radius: 10px; padding: 14px 16px; font-size: 12px; line-height: 1.75; color: #C5CEDA; white-space: pre-wrap; overflow-x: auto; max-height: 280px; overflow-y: auto">{{ log || 'No output.' }}</div>
      <p style="font-size: 13px; color: #C5CEDA">{{ note }} <NuxtLink v-if="selected.job_id" :to="`/activity?open=${selected.job_id}`">Full log</NuxtLink></p>
    </section>
  </div>

  <div v-if="gitProjects.length" style="display: flex; flex-wrap: wrap; gap: 16px; align-items: flex-start">
    <section class="card" aria-labelledby="pipe" style="flex: 1 1 420px; min-width: 0; padding: 20px; display: flex; flex-direction: column; gap: 14px">
      <div style="display: flex; align-items: center; justify-content: space-between; gap: 10px; flex-wrap: wrap">
        <h2 id="pipe" style="font-size: 17px; font-weight: 600">Pipeline · {{ pipe?.id ?? '' }}</h2>
        <SearchSelect v-model="pipeId" :options="gitProjects.map((p) => p.id)" aria-label="Project" placeholder="Project" style="width: 220px" />
      </div>
      <div style="display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px">
        <label style="display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Deploy when</span>
          <SearchSelect :model-value="form.auto_deploy ? 'push' : 'manual'" :options="[{ value: 'push', label: `Push to ${pipe?.branch ?? 'branch'}` }, { value: 'manual', label: 'Manual only' }]" aria-label="Deploy when" @update:model-value="(v) => form.auto_deploy = v === 'push'" />
        </label>
        <label style="display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Build on</span>
          <SearchSelect model-value="server" :options="[{ value: 'server', label: 'This server' }, { value: 'actions', label: 'GitHub Actions (see file →)' }]" aria-label="Build on" />
        </label>
      </div>
      <label v-for="t in toggles" :key="t.key" style="display: flex; align-items: center; gap: 12px; padding: 10px 0; border-bottom: 1px solid #1B222B; cursor: pointer">
        <span style="flex: 1; display: flex; flex-direction: column; gap: 2px"><span style="font-size: 14px; font-weight: 600">{{ t.name }}</span><span style="font-size: 12px; color: #A3AEBD">{{ t.hint }}</span></span>
        <input type="checkbox" :checked="t.on" :disabled="t.readonly" style="width: 20px; height: 20px; accent-color: #3DDC97" @change="setToggle(t.key, ($event.target as HTMLInputElement).checked)">
      </label>
      <div style="display: flex; flex-direction: column; gap: 8px">
        <div style="display: flex; align-items: center; justify-content: space-between"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Deploy script</span><button v-if="pipe?.kind === 'laravel'" type="button" class="btn btn-s" @click="form.script = DEFAULT_LARAVEL">Reset to Laravel default</button></div>
        <textarea v-model="form.script" class="mono" spellcheck="false" aria-label="Deploy script" placeholder="composer install --no-dev …" style="background: #07090C; border: 1px solid #1B222B; border-radius: 10px; padding: 14px 16px; font-size: 12px; line-height: 1.75; color: #C5CEDA; min-height: 200px; resize: vertical; width: 100%; box-sizing: border-box" />
      </div>
      <div style="display: flex; gap: 10px; align-items: center"><button type="button" class="btn btn-p" @click="savePipe">Save pipeline</button><span v-if="saved" style="font-size: 13px; color: #A3AEBD" role="status">{{ saved }}</span></div>
    </section>

    <section style="flex: 1 1 420px; min-width: 0; display: flex; flex-direction: column; gap: 16px">
      <div class="card" style="padding: 20px; display: flex; flex-direction: column; gap: 12px">
        <div style="display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px">
          <h2 style="font-size: 17px; font-weight: 600">Prefer GitHub Actions?</h2>
          <button type="button" class="btn btn-s" @click="copy(pipe?.actions_yaml ?? '')">Copy file</button>
        </div>
        <p style="font-size: 13px; color: #A3AEBD; line-height: 1.5">Add this file to the repo. Tests run on GitHub; NexaCloud only receives a green build. Add the repository secrets NEXACLOUD_URL and NEXACLOUD_SECRET (the webhook secret on the project page).</p>
        <div class="mono" style="background: #07090C; border: 1px solid #1B222B; border-radius: 10px; padding: 14px 16px; font-size: 12px; line-height: 1.75; color: #C5CEDA; white-space: pre; overflow-x: auto">{{ pipe?.actions_yaml ?? '' }}</div>
      </div>
      <div class="card" style="padding: 20px; display: flex; flex-direction: column; gap: 10px">
        <h2 style="font-size: 17px; font-weight: 600">Releases on disk</h2>
        <div v-for="r in releasesRes" :key="r.name" style="display: flex; align-items: center; gap: 12px; padding: 8px 0; border-bottom: 1px solid #1B222B">
          <span class="mono" style="flex: 1; font-size: 13px">releases/{{ r.name }}</span>
          <span style="font-size: 12px; color: #A3AEBD">{{ r.commit?.slice(0, 7) }}</span>
          <span v-if="r.current" class="pill ok">current</span>
          <button v-else type="button" class="btn btn-s" :disabled="activating === r.name" @click="activate(r.name)">{{ activating === r.name ? 'Activating…' : 'Activate' }}</button>
        </div>
        <p v-if="!releasesRes.length" style="font-size: 13px; color: #A3AEBD">{{ pipe?.strategy === 'releases' ? 'No releases yet — the next deploy creates releases/1.' : 'This project deploys in place. Switch to release folders on the project’s Deployments tab for instant rollbacks.' }}</p>
      </div>
    </section>
  </div>
</template>
