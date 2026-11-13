<script setup lang="ts">
import type { Backup, DatabasesInfo, Engine, Job } from '~/types/api'

useHead({ title: 'Databases · NexaCloud' })

const api = useApi()
const dbs = useResource<DatabasesInfo>('/api/databases')
const backups = useResource<Backup[]>('/api/backups')

const ENGINES: { id: Engine; label: string }[] = [
  { id: 'mysql', label: 'MySQL' },
  { id: 'postgres', label: 'PostgreSQL' },
]

const picked = ref<Engine | null>(null)
const engine = computed<Engine>(() => {
  if (picked.value) return picked.value
  const d = dbs.data.value
  if (d && !d.mysql.available && d.postgres.available) return 'postgres'
  return 'mysql'
})
const engineLabel = computed(() => ENGINES.find((e) => e.id === engine.value)?.label ?? engine.value)
const current = computed(() => dbs.data.value?.[engine.value] ?? null)
const totalSize = computed(() => (current.value?.databases ?? []).reduce((n, d) => n + d.size_bytes, 0))

const engineBackups = computed(() => (backups.data.value ?? []).filter((b) => b.engine === engine.value))

function lastBackup(name: string): Backup | null {
  // /api/backups is newest first.
  return engineBackups.value.find((b) => b.database === name) ?? null
}

function downloadUrl(file: string): string {
  return `/api/backups/download?file=${encodeURIComponent(file)}`
}

function fileName(file: string): string {
  return file.split('/').pop() ?? file
}

// ---- backup jobs ----
const watcher = useJobWatcher({
  interval: 1500,
  onFinish: (job: Job) => {
    if (job.status === 'ok') backups.refresh()
  },
})
/** "engine/name" → job id */
const jobFor = ref<Record<string, string>>({})
const startErrors = ref<Record<string, string>>({})
const starting = ref<Record<string, boolean>>({})
const openLogs = ref<Set<string>>(new Set())

function key(name: string) {
  return `${engine.value}/${name}`
}

function isRunning(name: string): boolean {
  const id = jobFor.value[key(name)]
  if (!id) return false
  const w = watcher.jobs.value[id]
  if (!w || w.lost) return false
  return !w.job || !isJobFinished(w.job.status)
}

async function backUp(name: string) {
  const k = key(name)
  const eng = engine.value
  starting.value = { ...starting.value, [k]: true }
  const errs = { ...startErrors.value }
  delete errs[k]
  startErrors.value = errs
  try {
    const res = await api.post<{ job_id: string }>(
      `/api/databases/${eng}/${encodeURIComponent(name)}/backup`,
    )
    jobFor.value = { ...jobFor.value, [k]: res.job_id }
    watcher.watch(res.job_id)
  } catch (err) {
    startErrors.value = { ...startErrors.value, [k]: errorMessage(err) }
  } finally {
    starting.value = { ...starting.value, [k]: false }
  }
}

const watched = computed(() => Object.values(watcher.jobs.value).reverse())

function dismiss(id: string) {
  watcher.dismiss(id)
  const next: Record<string, string> = {}
  for (const [k, v] of Object.entries(jobFor.value)) if (v !== id) next[k] = v
  jobFor.value = next
}

function toggleLog(id: string) {
  const next = new Set(openLogs.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  openLogs.value = next
}

function progressClass(job: Job | null): string {
  if (!job) return ''
  if (job.status === 'ok') return 'done'
  if (job.status === 'failed' || job.status === 'cancelled' || job.status === 'interrupted') return 'failed'
  return ''
}
</script>

<template>
  <PageHeader title="Databases & Backups" crumb="MySQL, PostgreSQL and database dumps">
    <div class="segmented" role="tablist" aria-label="Database engine">
      <button
        v-for="e in ENGINES"
        :id="`tab-${e.id}`"
        :key="e.id"
        type="button"
        role="tab"
        :aria-selected="engine === e.id"
        aria-controls="engine-panel"
        @click="picked = e.id"
      >{{ e.label }}</button>
    </div>
  </PageHeader>

  <!-- Running / finished backup jobs -->
  <section v-if="watched.length" aria-label="Backup progress" class="stack" style="gap: 10px">
    <div v-for="w in watched" :key="w.id" class="card job-card" :class="w.job?.status ?? 'queued'">
      <div class="row wrap">
        <StatusPill v-if="w.lost" tone="warn" label="Unknown" />
        <StatusPill v-else-if="w.job" v-bind="jobStatus(w.job.status)" />
        <StatusPill v-else tone="off" label="Starting" />
        <span class="grow job-title">{{ w.job?.title ?? 'Starting backup…' }}</span>
        <span v-if="w.job?.started_at" class="mono small text-2">
          {{ formatDuration(secondsBetween(w.job.started_at, w.job.finished_at)) }}
        </span>
        <button
          v-if="w.job?.log"
          type="button"
          class="btn btn-s"
          :aria-expanded="openLogs.has(w.id)"
          @click="toggleLog(w.id)"
        >{{ openLogs.has(w.id) ? 'Hide output' : 'Show output' }}</button>
        <button
          v-if="w.lost || (w.job && isJobFinished(w.job.status))"
          type="button"
          class="icon-btn"
          aria-label="Dismiss"
          @click="dismiss(w.id)"
        ><AppIcon name="close" :size="14" :stroke="2" /></button>
      </div>
      <div
        class="progress"
        :class="progressClass(w.job)"
        role="progressbar"
        :aria-label="w.job?.title ?? 'Backup'"
        :aria-busy="!w.job || !isJobFinished(w.job.status)"
      ><span /></div>
      <p v-if="w.lost" class="small" style="color: var(--warn)">{{ w.lost }}</p>
      <p v-else-if="w.job?.status === 'ok'" class="small" style="color: var(--ok)">Backup finished. It's in the list below.</p>
      <p v-else-if="w.job?.status === 'failed'" class="small" style="color: var(--bad)">Backup failed. Show the output for details.</p>
      <p v-else-if="w.job?.status === 'cancelled'" class="small muted">Backup cancelled.</p>
      <p v-else-if="w.job?.status === 'interrupted'" class="small" style="color: var(--warn)">The agent restarted before this backup finished. Start it again.</p>
      <pre v-if="w.job?.log && openLogs.has(w.id)" class="logbox">{{ w.job.log }}</pre>
    </div>
  </section>

  <div id="engine-panel" role="tabpanel" :aria-labelledby="`tab-${engine}`" class="stack" style="gap: 22px">
    <div v-if="dbs.loading.value" class="card card-pad"><SkeletonRows :rows="5" label="Loading databases" /></div>
    <ErrorBox
      v-else-if="dbs.error.value && !dbs.data.value"
      :message="dbs.error.value"
      :retrying="dbs.refreshing.value"
      @retry="dbs.refresh()"
    />
    <div v-else-if="current && !current.available" class="card empty">
      <AppIcon name="databases" :size="28" class="muted" />
      <template v-if="current.error">
        <strong>{{ engineLabel }} is installed but NexaCloud can't connect</strong>
        <span>{{ current.error }}</span>
      </template>
      <template v-else>
        <strong>{{ engineLabel }} isn't installed</strong>
        <span>No local {{ engineLabel }} server was found on this machine.</span>
      </template>
    </div>
    <template v-else-if="current">
      <section aria-label="Engine stats" class="grid-cards" style="grid-template-columns: repeat(auto-fit, minmax(170px, 1fr)); gap: 12px">
        <div class="card mini">
          <span class="lbl">Version</span>
          <span class="mini-num">{{ current.version ?? '—' }}</span>
        </div>
        <div class="card mini">
          <span class="lbl">Databases</span>
          <span class="mini-num">{{ current.databases.length }}</span>
        </div>
        <div class="card mini">
          <span class="lbl">Total size</span>
          <span class="mini-num">{{ formatBytes(totalSize) }}</span>
        </div>
        <div class="card mini">
          <span class="lbl">Backups on disk</span>
          <span class="mini-num">{{ backups.data.value ? engineBackups.length : '—' }}</span>
        </div>
      </section>

      <section class="card" aria-labelledby="dbl" style="overflow: hidden">
        <div class="card-head"><h2 id="dbl">{{ engineLabel }} databases</h2></div>
        <div v-if="!current.databases.length" class="empty">
          <strong>No databases yet</strong>
          <span>System schemas are hidden.</span>
        </div>
        <div v-else class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Database</th>
                <th class="num">Size</th>
                <th class="num">Tables</th>
                <th>Last backup</th>
                <th><span class="sr-only">Actions</span></th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="d in current.databases" :key="d.name">
                <td class="mono" style="font-weight: 500"><NuxtLink :to="`/db/${engine}/${encodeURIComponent(d.name)}`">{{ d.name }}</NuxtLink></td>
                <td class="mono num" style="font-size: 13px">{{ formatBytes(d.size_bytes) }}</td>
                <td class="mono num" style="font-size: 13px">{{ d.tables }}</td>
                <td>
                  <span v-if="!backups.data.value" class="muted">—</span>
                  <span v-else-if="lastBackup(d.name)" :title="formatDateTime(lastBackup(d.name)!.created_at)" class="text-2" style="font-size: 13px">
                    {{ formatRelative(lastBackup(d.name)!.created_at) }}
                  </span>
                  <StatusPill v-else tone="warn" label="Never" />
                </td>
                <td>
                  <div class="row" style="gap: 10px; justify-content: flex-end">
                    <span v-if="startErrors[key(d.name)]" class="small" style="color: var(--bad); white-space: normal">
                      {{ startErrors[key(d.name)] }}
                    </span>
                    <button
                      type="button"
                      class="btn btn-s"
                      :disabled="starting[key(d.name)] || isRunning(d.name)"
                      @click="backUp(d.name)"
                    >
                      <span v-if="starting[key(d.name)] || isRunning(d.name)" class="spinner" aria-hidden="true" />
                      <AppIcon v-else name="backup" :size="14" :stroke="2" />
                      {{ isRunning(d.name) ? 'Backing up…' : 'Back up now' }}
                    </button>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>
    </template>
  </div>

  <!-- Backups -->
  <section class="card" aria-labelledby="bk-h" style="overflow: hidden">
    <div class="card-head">
      <div class="cell-stack">
        <h2 id="bk-h">Backups</h2>
        <span class="small muted">Compressed SQL dumps (<span class="mono">.sql.gz</span>) stored on this server, newest first.</span>
      </div>
      <button type="button" class="btn btn-s" :disabled="backups.refreshing.value" @click="backups.refresh()">
        <span v-if="backups.refreshing.value" class="spinner" aria-hidden="true" />
        <AppIcon v-else name="restart" :size="14" :stroke="2" />
        Refresh
      </button>
    </div>
    <div v-if="backups.loading.value" style="padding: 0 20px 20px"><SkeletonRows :rows="4" label="Loading backups" /></div>
    <div v-else-if="backups.error.value && !backups.data.value" style="padding: 0 20px 20px">
      <ErrorBox :message="backups.error.value" :retrying="backups.refreshing.value" @retry="backups.refresh()" />
    </div>
    <div v-else-if="!backups.data.value?.length" class="empty">
      <strong>No backups yet</strong>
      <span>Use “Back up now” on a database above to create the first one.</span>
    </div>
    <div v-else class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Database</th>
            <th>File</th>
            <th class="num">Size</th>
            <th>Created</th>
            <th><span class="sr-only">Download</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="b in backups.data.value" :key="b.file">
            <td>
              <div class="row" style="gap: 8px">
                <span class="tag">{{ b.engine === 'postgres' ? 'PostgreSQL' : b.engine === 'mysql' ? 'MySQL' : b.engine }}</span>
                <span class="mono" style="font-weight: 500">{{ b.database }}</span>
              </div>
            </td>
            <td class="mono small text-2" :title="b.file">{{ fileName(b.file) }}</td>
            <td class="mono num" style="font-size: 13px">{{ formatBytes(b.size_bytes) }}</td>
            <td>
              <div class="cell-stack">
                <span style="font-size: 13px">{{ formatDateTime(b.created_at) }}</span>
                <span class="cell-sub">{{ formatRelative(b.created_at) }}</span>
              </div>
            </td>
            <td style="text-align: right">
              <a class="btn btn-s" :href="downloadUrl(b.file)" download :aria-label="`Download ${fileName(b.file)}`">
                <AppIcon name="download" :size="14" :stroke="2" />
                Download
              </a>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
  <DbAdmin v-if="current?.available" :engine="engine" @changed="dbs.refresh()" />
</template>

<style scoped>
.mini { padding: 16px; display: flex; flex-direction: column; gap: 6px; }
.mini-num { font-family: var(--font-head); font-size: 24px; font-weight: 600; overflow-wrap: anywhere; }
.job-card { padding: 16px 20px; display: flex; flex-direction: column; gap: 12px; border-color: #24405A; background: #0F1822; }
.job-card.ok { border-color: #1D3A2D; background: #0E1713; }
.job-card.failed { border-color: #4A2326; background: #1A1012; }
.job-title { font-size: 15px; font-weight: 600; }
</style>
