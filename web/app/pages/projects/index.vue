<script setup lang="ts">
import type { Site } from '~/types/api'

useHead({ title: 'Deployments · NexaCloud' })

interface DeployRow { id: string; project_id: string; status: string; trigger: string; commit: string | null; message: string | null; author: string | null; started_at: string; finished_at: string | null; error: string | null }
interface ProjectRow { id: string; kind: string; dir: string; repo: string; branch: string; auto_deploy: boolean; last_commit: string | null; last_deploy_at: string | null; last: DeployRow | null }

const api = useApi()
const projects = useResource<ProjectRow[]>('/api/projects', { interval: 15_000 })
const deploys = useResource<DeployRow[]>('/api/deploys', { interval: 15_000 })
const sites = useResource<Site[]>('/api/sites')

const connecting = ref(false)
const form = reactive({ site_id: '', repo: '', branch: 'main', migrate: true, auto_deploy: true })
const candidates = computed(() => (sites.data.value ?? []).filter((s) => s.root && !(projects.data.value ?? []).some((p) => p.id === s.id)))
watch(() => form.site_id, (id) => {
  const s = sites.data.value?.find((x) => x.id === id)
  if (s?.git?.remote && !form.repo) form.repo = s.git.remote
  if (s?.git?.branch) form.branch = s.git.branch
})
const err = ref<string | null>(null)
async function connect() {
  err.value = null
  try {
    const p = await api.post<{ id: string }>('/api/projects', { ...form })
    await navigateTo(`/projects/${encodeURIComponent(p.id)}?new=1`)
  } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) }
}
function deployTone(s?: string) { return s === 'ok' ? 'ok' : s === 'running' || s === 'queued' ? 'info' : s ? 'bad' : 'off' }
function deployLabel(s?: string) { return ({ ok: 'Live', running: 'Deploying', queued: 'Queued', failed: 'Failed', rolled_back: 'Rolled back' } as Record<string, string>)[s ?? ''] ?? 'Never deployed' }
</script>

<template>
  <PageHeader title="Deployments" crumb="Connect sites to Git and deploy on push">
    <button type="button" class="btn btn-p" @click="connecting = !connecting"><AppIcon name="git" :size="16" :stroke="2" /> Connect a site</button>
  </PageHeader>

  <section v-if="connecting" class="card card-pad stack" style="gap: 14px" aria-label="Connect a site to Git">
    <h2>Connect a site to a Git repository</h2>
    <div class="fgrid">
      <label class="field"><span>Site</span>
        <select v-model="form.site_id" class="in"><option value="" disabled>Choose a site</option><option v-for="s in candidates" :key="s.id" :value="s.id">{{ s.id }} ({{ siteKindLabel(s.kind) }})</option></select>
      </label>
      <label class="field"><span>Repository</span><input v-model="form.repo" class="in mono" type="text" placeholder="git@github.com:owner/repo.git" spellcheck="false"></label>
      <label class="field"><span>Branch</span><input v-model="form.branch" class="in mono" type="text"></label>
    </div>
    <label class="small"><input v-model="form.auto_deploy" type="checkbox"> Deploy automatically when I push (checks every minute; instant with the webhook)</label>
    <label class="small"><input v-model="form.migrate" type="checkbox"> Run database migrations (Laravel; a backup is taken first)</label>
    <p class="small muted">NexaCloud creates a read-only deploy key for this repo — you'll add it to GitHub on the next screen. Deploys run as the site's user, never as root.</p>
    <p v-if="err" class="small" style="color: var(--bad)">{{ err }}</p>
    <div><button type="button" class="btn btn-p" :disabled="!form.site_id || !form.repo" @click="connect">Connect</button></div>
  </section>

  <section class="card" aria-labelledby="proj-h" style="overflow: hidden">
    <div class="card-head"><h2 id="proj-h">Projects</h2></div>
    <div v-if="projects.loading.value" style="padding: 0 20px 20px"><SkeletonRows :rows="4" /></div>
    <div v-else-if="!projects.data.value?.length" class="empty">
      <strong>No sites connected to Git yet</strong>
      <span>Connect a site to deploy it from GitHub with one click — or automatically on every push.</span>
      <button type="button" class="btn btn-s btn-p" @click="connecting = true">Connect a site</button>
    </div>
    <div v-else class="table-wrap"><table>
      <thead><tr><th>Project</th><th>Repository</th><th>Status</th><th>Last deploy</th><th>Auto</th><th /></tr></thead>
      <tbody>
        <tr v-for="p in projects.data.value" :key="p.id">
          <td><NuxtLink :to="`/projects/${encodeURIComponent(p.id)}`" style="font-weight: 600">{{ p.id }}</NuxtLink><div class="small muted">{{ p.kind }}</div></td>
          <td class="mono small">{{ p.repo }}<div class="muted">{{ p.branch }}</div></td>
          <td><StatusPill :tone="deployTone(p.last?.status)" :label="deployLabel(p.last?.status)" /></td>
          <td class="small">
            <template v-if="p.last"><span class="mono">{{ p.last.commit?.slice(0, 8) }}</span> {{ p.last.message }}<div class="muted">{{ formatRelative(p.last.started_at) }} · {{ p.last.trigger }}</div></template>
            <span v-else class="muted">—</span>
          </td>
          <td><StatusPill :tone="p.auto_deploy ? 'ok' : 'off'" :label="p.auto_deploy ? 'On push' : 'Manual'" /></td>
          <td><NuxtLink :to="`/projects/${encodeURIComponent(p.id)}`" class="btn btn-s">Open</NuxtLink></td>
        </tr>
      </tbody>
    </table></div>
  </section>

  <section class="card card-pad stack" aria-labelledby="recent-h" style="gap: 8px">
    <h2 id="recent-h">Recent deploys</h2>
    <div v-for="d in deploys.data.value ?? []" :key="d.id" class="drow">
      <StatusPill :tone="deployTone(d.status)" :label="deployLabel(d.status)" />
      <NuxtLink :to="`/projects/${encodeURIComponent(d.project_id)}`" style="font-weight: 600">{{ d.project_id }}</NuxtLink>
      <span class="mono small">{{ d.commit?.slice(0, 8) }}</span>
      <span class="small text-2 grow">{{ d.message ?? d.error }}</span>
      <span class="small muted">{{ d.trigger }} · {{ formatRelative(d.started_at) }}</span>
    </div>
    <span v-if="!deploys.data.value?.length" class="small muted">No deploys yet.</span>
  </section>
</template>

<style scoped>
.fgrid { display: grid; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); gap: 12px; }
.field { display: flex; flex-direction: column; gap: 6px; }
.field > span:first-child { font-size: 13px; font-weight: 500; color: var(--text-2); }
.in { min-height: 40px; padding: 0 10px; background: var(--sunken, #0F1318); border: 1px solid var(--btn-line, #2A3441); border-radius: 8px; color: var(--text); font-size: 14px; width: 100%; box-sizing: border-box; }
input[type=checkbox] { accent-color: var(--accent); }
.drow { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; padding: 8px 0; border-bottom: 1px solid #1B222B; }
.grow { flex: 1 1 200px; min-width: 0; }
</style>
