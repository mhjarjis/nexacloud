<script setup lang="ts">
import type { Job } from '~/types/api'

useHead({ title: 'Jobs · NexaCloud' })

const jobs = useResource<Job[]>('/api/jobs', { interval: 5000 })
const open = ref<Set<string>>(new Set())

function toggle(id: string) {
  const next = new Set(open.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  open.value = next
}

const KIND_LABELS: Record<string, string> = { db_backup: 'Database backup' }
</script>

<template>
  <PageHeader title="Jobs" crumb="Background tasks run by the agent">
    <span class="small muted">Last 50 · cleared when the agent restarts</span>
  </PageHeader>

  <section class="card" aria-labelledby="jobs-h" style="overflow: hidden">
    <div class="card-head"><h2 id="jobs-h">Recent jobs</h2></div>
    <div v-if="jobs.loading.value" style="padding: 0 20px 20px"><SkeletonRows :rows="5" label="Loading jobs" /></div>
    <div v-else-if="jobs.error.value && !jobs.data.value" style="padding: 0 20px 20px">
      <ErrorBox :message="jobs.error.value" :retrying="jobs.refreshing.value" @retry="jobs.refresh()" />
    </div>
    <div v-else-if="!jobs.data.value?.length" class="empty">
      <strong>No jobs yet</strong>
      <span>Backups you start from <NuxtLink to="/databases">Databases &amp; Backups</NuxtLink> show up here.</span>
    </div>
    <ul v-else class="job-list">
      <li v-for="j in jobs.data.value" :key="j.id" class="job" :class="{ open: open.has(j.id) }">
        <button
          type="button"
          class="job-head"
          :aria-expanded="open.has(j.id)"
          :aria-controls="`log-${j.id}`"
          @click="toggle(j.id)"
        >
          <AppIcon name="chevron" :size="16" :stroke="2" class="chev muted" />
          <StatusPill v-bind="jobStatus(j.status)" />
          <span class="grow job-text">
            <span class="job-title">{{ j.title }}</span>
            <span class="small muted">{{ KIND_LABELS[j.kind] ?? j.kind }}<template v-if="j.started_at"> · started {{ formatRelative(j.started_at) }}</template></span>
          </span>
          <span v-if="j.started_at" class="mono small text-2 job-dur">
            <span v-if="!isJobFinished(j.status)" class="spinner" aria-hidden="true" style="width: 12px; height: 12px" />
            {{ formatDuration(secondsBetween(j.started_at, j.finished_at)) }}
          </span>
        </button>
        <div v-if="open.has(j.id)" :id="`log-${j.id}`" class="job-body">
          <div class="row wrap small muted" style="gap: 16px">
            <span>Started: {{ formatDateTime(j.started_at) }}</span>
            <span>Finished: {{ j.finished_at ? formatDateTime(j.finished_at) : 'not yet' }}</span>
            <span class="mono">{{ j.id }}</span>
          </div>
          <pre v-if="j.log" class="logbox">{{ j.log }}</pre>
          <p v-else class="small muted">No output yet.</p>
        </div>
      </li>
    </ul>
  </section>
</template>

<style scoped>
.job-list { list-style: none; margin: 0; padding: 0; }
.job { border-top: 1px solid var(--row-line); }
.job-head {
  display: flex; align-items: center; gap: 12px; width: 100%; min-height: 60px; padding: 10px 20px;
  background: transparent; border: 0; color: var(--text); text-align: left; cursor: pointer;
}
.job-head:hover, .job.open .job-head { background: #141A21; }
.job-text { display: flex; flex-direction: column; gap: 2px; }
.job-title { font-size: 14px; font-weight: 600; overflow-wrap: anywhere; }
.job-dur { display: inline-flex; align-items: center; gap: 6px; }
.job-head .pill { min-width: 78px; justify-content: center; }
.chev { transition: transform .15s ease; flex: none; }
.job.open .chev { transform: rotate(90deg); }
.job-body { padding: 4px 20px 18px 48px; display: flex; flex-direction: column; gap: 10px; background: #141A21; }
@media (max-width: 700px) { .job-body { padding-left: 20px; } }
</style>
