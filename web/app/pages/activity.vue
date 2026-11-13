<script setup lang="ts">
// One timeline of changes (with their plans, diffs and rollback) and
// background jobs, newest first. A change's own apply job is folded into it.
import type { ChangeDetail, ChangeStarted, ChangeSummary, Job, JobStatus } from '~/types/api'

useHead({ title: 'Activity · NexaCloud' })

const api = useApi()
const route = useRoute()
const changes = useResource<ChangeSummary[]>('/api/changes', { interval: 5000 })
const jobs = useResource<Job[]>('/api/jobs', { interval: 5000 })

type Item =
  | { type: 'change'; id: string; at: string; change: ChangeSummary; job: Job | null }
  | { type: 'job'; id: string; at: string; job: Job }

const items = computed<Item[]>(() => {
  const cs = changes.data.value ?? []
  const js = jobs.data.value ?? []
  const jobById = new Map(js.map((j) => [j.id, j]))
  const changeIds = new Set(cs.map((c) => c.id))
  const out: Item[] = cs.map((c) => ({
    type: 'change', id: c.id, at: c.created_at, change: c, job: c.job_id ? jobById.get(c.job_id) ?? null : null,
  }))
  for (const j of js) {
    if (j.change_id && changeIds.has(j.change_id)) continue
    out.push({ type: 'job', id: j.id, at: j.created_at, job: j })
  }
  return out.sort((a, b) => (a.at < b.at ? 1 : a.at > b.at ? -1 : 0))
})

const loading = computed(() => changes.loading.value || jobs.loading.value)
const loadError = computed(() =>
  (!changes.data.value && changes.error.value) || (!jobs.data.value && jobs.error.value) || null)

async function refreshAll() {
  await Promise.all([changes.refresh(), jobs.refresh()])
}

// ---- expanded items + change details
const open = ref<Set<string>>(new Set())
const details = ref<Record<string, ChangeDetail>>({})
const detailError = ref<Record<string, string>>({})

async function loadDetail(id: string) {
  try {
    const d = await api.get<ChangeDetail>(`/api/changes/${encodeURIComponent(id)}`)
    details.value = { ...details.value, [id]: d }
    const next = { ...detailError.value }
    delete next[id]
    detailError.value = next
  } catch (err) {
    detailError.value = { ...detailError.value, [id]: errorMessage(err) }
  }
}

function toggle(it: Item) {
  const next = new Set(open.value)
  if (next.has(it.id)) next.delete(it.id)
  else {
    next.add(it.id)
    if (it.type === 'change') loadDetail(it.id)
  }
  open.value = next
}

// Keep open change details fresh when their status moves.
watch(() => changes.data.value, (list) => {
  for (const c of list ?? []) {
    const d = details.value[c.id]
    if (open.value.has(c.id) && d && (d.status !== c.status || d.can_rollback !== c.can_rollback)) loadDetail(c.id)
  }
})

// Deep link: /activity?open=<change or job id> — opened once, then the
// person can collapse it without the next poll re-opening it.
let deepLinkDone = false
watch(items, (list) => {
  const want = typeof route.query.open === 'string' ? route.query.open : null
  if (deepLinkDone || !want) return
  const it = list.find((i) => i.id === want)
  if (!it) return
  deepLinkDone = true
  if (!open.value.has(want)) toggle(it)
}, { immediate: true })

function onJobFinished(_status: JobStatus) {
  refreshAll()
}

// ---- actions
const busy = ref<Record<string, boolean>>({})
const actionError = ref<Record<string, string>>({})
const drift = ref<Record<string, boolean>>({})

function setBusy(id: string, v: boolean) { busy.value = { ...busy.value, [id]: v } }
function setError(id: string, msg: string | null) {
  const next = { ...actionError.value }
  if (msg) next[id] = msg
  else delete next[id]
  actionError.value = next
}

async function rollback(c: ChangeSummary, force = false) {
  setBusy(c.id, true)
  setError(c.id, null)
  try {
    const res = await api.post<ChangeStarted>(`/api/changes/${encodeURIComponent(c.id)}/rollback`, { force })
    drift.value = { ...drift.value, [c.id]: false }
    await refreshAll()
    const next = new Set(open.value)
    next.add(res.change_id)
    open.value = next
    loadDetail(res.change_id)
    loadDetail(c.id)
  } catch (err) {
    if (err instanceof SudoCancelled) return
    const msg = errorMessage(err)
    if (err instanceof ApiError && err.status === 409 && /changed since/.test(msg)) drift.value = { ...drift.value, [c.id]: true }
    setError(c.id, msg)
  } finally {
    setBusy(c.id, false)
  }
}

async function cancelJob(j: Job) {
  setBusy(j.id, true)
  setError(j.id, null)
  try {
    await api.post(`/api/jobs/${encodeURIComponent(j.id)}/cancel`)
    await refreshAll()
  } catch (err) {
    if (!(err instanceof SudoCancelled)) setError(j.id, errorMessage(err))
  } finally {
    setBusy(j.id, false)
  }
}

// ---- labels
const KIND_LABELS: Record<string, string> = {
  db_backup: 'Database backup',
  change_apply: 'Change',
  change_rollback: 'Rollback',
  nginx_hardening: 'nginx setting',
  rollback: 'Rollback',
  test_sleep: 'Test job',
  test_broken: 'Test change',
}
const kindLabel = (k: string) => KIND_LABELS[k] ?? k.replace(/_/g, ' ')
const who = (ip: string | null) => (ip === 'cli' ? 'command line' : ip ?? 'NexaCloud')

function itemStatus(it: Item) {
  return it.type === 'change' ? changeStatus(it.change.status) : jobStatus(it.job.status)
}
function itemRunning(it: Item): boolean {
  return it.type === 'change' ? !isChangeFinished(it.change.status) : !isJobFinished(it.job.status)
}
function itemDuration(it: Item): string | null {
  const j = it.type === 'change' ? it.job : it.job
  if (j?.started_at) return formatDuration(secondsBetween(j.started_at, j.finished_at))
  return null
}
function canCancel(j: Job): boolean {
  return j.status === 'queued' || (j.status === 'running' && j.cancellable)
}
function titleOf(id: string | null): string {
  if (!id) return ''
  return changes.data.value?.find((c) => c.id === id)?.title ?? id.slice(0, 8)
}
</script>

<template>
  <PageHeader title="Activity" crumb="Every change NexaCloud made and every background job">
    <span class="small muted">Newest first · kept across restarts</span>
  </PageHeader>

  <section class="card" aria-labelledby="act-h" style="overflow: hidden">
    <div class="card-head"><h2 id="act-h">Timeline</h2></div>
    <div v-if="loading" style="padding: 0 20px 20px"><SkeletonRows :rows="5" label="Loading activity" /></div>
    <div v-else-if="loadError" style="padding: 0 20px 20px">
      <ErrorBox :message="loadError" :retrying="changes.refreshing.value || jobs.refreshing.value" @retry="refreshAll()" />
    </div>
    <div v-else-if="!items.length" class="empty">
      <strong>Nothing yet</strong>
      <span>Changes you apply (for example from <NuxtLink to="/security">Security</NuxtLink>) and backups you start from <NuxtLink to="/databases">Databases &amp; Backups</NuxtLink> show up here.</span>
    </div>
    <ul v-else class="act-list">
      <li v-for="it in items" :key="it.id" class="act" :class="{ open: open.has(it.id) }" :data-id="it.id">
        <button
          type="button"
          class="act-head"
          :aria-expanded="open.has(it.id)"
          :aria-controls="`act-${it.id}`"
          @click="toggle(it)"
        >
          <AppIcon name="chevron" :size="16" :stroke="2" class="chev muted" />
          <StatusPill v-bind="itemStatus(it)" />
          <span class="grow act-text">
            <span class="act-title">
              <AppIcon v-if="it.type === 'change'" :name="it.change.kind === 'rollback' ? 'undo' : 'file'" :size="14" :stroke="2" class="muted" />
              {{ it.type === 'change' ? it.change.title : it.job.title }}
            </span>
            <span class="small muted">
              {{ it.type === 'change' ? kindLabel(it.change.kind) : kindLabel(it.job.kind) }}
              · {{ formatRelative(it.at) }}
              · by {{ who(it.type === 'change' ? it.change.actor_ip : it.job.actor_ip) }}
            </span>
          </span>
          <span class="mono small text-2 act-dur">
            <span v-if="itemRunning(it)" class="spinner" aria-hidden="true" style="width: 12px; height: 12px" />
            {{ itemDuration(it) ?? '' }}
          </span>
        </button>

        <div v-if="open.has(it.id)" :id="`act-${it.id}`" class="act-body">
          <!-- ===== change ===== -->
          <template v-if="it.type === 'change'">
            <div class="row wrap small muted" style="gap: 16px">
              <span>Started: {{ formatDateTime(it.change.created_at) }}</span>
              <span>Finished: {{ it.change.finished_at ? formatDateTime(it.change.finished_at) : 'not yet' }}</span>
              <span>Target: <span class="mono">{{ it.change.target }}</span></span>
              <span class="mono">{{ it.change.id }}</span>
            </div>
            <p v-if="it.change.rollback_of" class="small text-2">Rolls back <strong>{{ titleOf(it.change.rollback_of) }}</strong>.</p>
            <p v-if="it.change.rolled_back_by" class="small text-2">Rolled back later (change <span class="mono">{{ it.change.rolled_back_by.slice(0, 8) }}</span>).</p>
            <p v-if="it.change.error" class="act-error" :class="it.change.status === 'reverted' ? 'warn' : 'bad'">
              <template v-if="it.change.status === 'reverted'">Reverted automatically: </template>{{ it.change.error }}
            </p>

            <ErrorBox v-if="detailError[it.id] && !details[it.id]" :message="detailError[it.id]!" @retry="loadDetail(it.id)" />
            <SkeletonRows v-else-if="!details[it.id]" :rows="3" label="Loading change" />
            <template v-else>
              <p v-if="details[it.id]!.note" class="small" style="color: var(--warn)">{{ details[it.id]!.note }}</p>
              <div v-if="details[it.id]!.files.length" class="stack" style="gap: 8px">
                <span class="lbl">Files ({{ details[it.id]!.files.length }})</span>
                <details v-for="f in details[it.id]!.files" :key="f.path" class="act-file">
                  <summary>
                    <AppIcon name="chevron" :size="14" :stroke="2" class="chev muted" />
                    <span class="mono act-path">{{ f.path }}</span>
                    <span class="tag">{{ f.action }}</span>
                  </summary>
                  <DiffView v-if="f.diff" :diff="f.diff" />
                </details>
              </div>
              <div v-if="details[it.id]!.commands.length" class="stack" style="gap: 6px">
                <span class="lbl">Commands</span>
                <ol class="act-cmds mono">
                  <li v-for="(c, i) in details[it.id]!.commands" :key="i">{{ displayArgv(c) }}</li>
                </ol>
              </div>
              <div v-if="details[it.id]!.verify_results.length" class="stack" style="gap: 6px">
                <span class="lbl">Checks</span>
                <ul class="act-checks">
                  <li v-for="(v, i) in details[it.id]!.verify_results" :key="i" class="row" style="gap: 8px">
                    <StatusPill :tone="v.ok ? 'ok' : 'bad'" :label="v.ok ? 'Passed' : 'Failed'" />
                    <span class="mono small">{{ v.step }}</span>
                    <span class="small muted">{{ v.detail }}</span>
                  </li>
                </ul>
              </div>
            </template>

            <JobLog
              v-if="it.job"
              :key="`${it.job.id}-${isJobFinished(it.job.status)}`"
              :job-id="it.job.id"
              :status="it.job.status"
              :log="it.job.log"
              @finished="onJobFinished"
            />

            <div v-if="it.change.can_rollback" class="row wrap" style="gap: 10px">
              <ConfirmAction
                label="Rollback"
                :question="`Undo “${it.change.title}”?`"
                confirm-label="Roll back"
                :busy="busy[it.id]"
                :aria-label="`Roll back ${it.change.title}`"
                @confirm="rollback(it.change)"
              >
                <template #icon><AppIcon v-if="!busy[it.id]" name="undo" :size="14" :stroke="2" /></template>
              </ConfirmAction>
              <ConfirmAction
                v-if="drift[it.id]"
                label="Roll back anyway"
                question="Overwrite the later edits?"
                confirm-label="Overwrite"
                :busy="busy[it.id]"
                @confirm="rollback(it.change, true)"
              />
              <span class="small muted">Restores the files from the snapshot taken before this change.</span>
            </div>
            <p v-if="actionError[it.id]" class="small" style="color: var(--bad)" role="alert">{{ actionError[it.id] }}</p>
          </template>

          <!-- ===== job ===== -->
          <template v-else>
            <div class="row wrap small muted" style="gap: 16px">
              <span>Queued: {{ formatDateTime(it.job.created_at) }}</span>
              <span>Finished: {{ it.job.finished_at ? formatDateTime(it.job.finished_at) : 'not yet' }}</span>
              <span v-if="it.job.locks.length">Locks: <span class="mono">{{ it.job.locks.join(', ') }}</span></span>
              <span class="mono">{{ it.job.id }}</span>
            </div>
            <p v-if="it.job.error && it.job.status !== 'ok'" class="act-error" :class="it.job.status === 'failed' ? 'bad' : 'warn'">{{ it.job.error }}</p>
            <JobLog
              :key="`${it.job.id}-${isJobFinished(it.job.status)}`"
              :job-id="it.job.id"
              :status="it.job.status"
              :log="it.job.log"
              @finished="onJobFinished"
            />
            <div v-if="canCancel(it.job)" class="row wrap" style="gap: 10px">
              <ConfirmAction
                label="Cancel job"
                question="Stop this job?"
                confirm-label="Stop it"
                :busy="busy[it.id]"
                :aria-label="`Cancel ${it.job.title}`"
                @confirm="cancelJob(it.job)"
              />
            </div>
            <p v-if="actionError[it.id]" class="small" style="color: var(--bad)" role="alert">{{ actionError[it.id] }}</p>
          </template>
        </div>
      </li>
    </ul>
  </section>
</template>

<style scoped>
.act-list { list-style: none; margin: 0; padding: 0; }
.act { border-top: 1px solid var(--row-line); }
.act-head {
  display: flex; align-items: center; gap: 12px; width: 100%; min-height: 60px; padding: 10px 20px;
  background: transparent; border: 0; color: var(--text); text-align: left; cursor: pointer;
}
.act-head:hover, .act.open .act-head { background: #141A21; }
.act-text { display: flex; flex-direction: column; gap: 2px; }
.act-title { font-size: 14px; font-weight: 600; overflow-wrap: anywhere; display: inline-flex; align-items: center; gap: 6px; }
.act-dur { display: inline-flex; align-items: center; gap: 6px; flex: none; }
.act-head .pill { min-width: 92px; justify-content: center; }
.chev { transition: transform .15s ease; flex: none; }
.act.open > .act-head .chev { transform: rotate(90deg); }
.act-body { padding: 4px 20px 18px 48px; display: flex; flex-direction: column; gap: 12px; background: #141A21; }
.act-error { font-size: 13px; padding: 10px 12px; border-radius: 8px; overflow-wrap: anywhere; }
.act-file { border: 1px solid var(--row-line); border-radius: 10px; background: var(--sunken); }
.act-file > summary { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; padding: 10px 12px; cursor: pointer; list-style: none; }
.act-file > summary::-webkit-details-marker { display: none; }
.act-file[open] > summary { border-bottom: 1px solid var(--row-line); }
.act-file[open] .chev { transform: rotate(90deg); }
.act-file :deep(.diff) { border: 0; border-radius: 0 0 10px 10px; }
.act-path { font-size: 13px; overflow-wrap: anywhere; min-width: 0; }
.act-cmds { margin: 0; padding-left: 22px; font-size: 12px; color: var(--text-2); display: flex; flex-direction: column; gap: 4px; }
.act-checks { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 6px; }
.act-checks li { flex-wrap: wrap; }
@media (max-width: 700px) {
  .act-body { padding-left: 20px; }
  .act-head { gap: 8px; padding: 10px 14px; }
  .act-head .pill { min-width: 0; }
}
</style>
