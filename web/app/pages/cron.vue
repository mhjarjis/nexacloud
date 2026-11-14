<script setup lang="ts">
useHead({ title: 'Cron jobs · NexaCloud' })

interface CronJob { id: string; project_id: string | null; user: string; schedule: string; command: string; cwd: string; enabled: boolean; created_at: string; last_start: number | null; last_secs: number | null; last_exit: number | null }

const api = useApi()
const route = useRoute()
const jobs = useResource<CronJob[]>('/api/cron', { interval: 30_000 })
const me = useResource<{ role: string }>('/api/auth/me')
const projects = useResource<{ id: string; kind: string }[]>('/api/projects')
const host = computed(() => (typeof window !== 'undefined' ? window.location.hostname : 'server'))

const PRESETS = [
  { value: '* * * * *', label: 'Every minute', hint: '* * * * *' },
  { value: '*/5 * * * *', label: 'Every 5 minutes', hint: '*/5 * * * *' },
  { value: '*/15 * * * *', label: 'Every 15 minutes', hint: '*/15 * * * *' },
  { value: '0 * * * *', label: 'Every hour', hint: '0 * * * *' },
  { value: '0 3 * * *', label: 'Every day at 03:00', hint: '0 3 * * *' },
  { value: '0 3 * * 1', label: 'Every Monday at 03:00', hint: '0 3 * * 1' },
  { value: '0 4 1 * *', label: 'First day of the month', hint: '0 4 1 * *' },
  { value: '@reboot', label: 'When the server starts', hint: '@reboot' },
]
function describe(s: string): string {
  return PRESETS.find((p) => p.value === s)?.label ?? s
}

const targetOpts = computed(() => [
  ...(me.data.value?.role === 'admin' ? [{ value: '', label: 'Server (no project)', hint: 'runs as root or a user you choose' }] : []),
  ...(projects.data.value ?? []).map((p) => ({ value: p.id, label: p.id, hint: `runs as the project's user, in its folder` })),
])

const form = reactive({ open: false, id: '' as string, project: '' as string | number | null, user: 'root', preset: '*/5 * * * *' as string | number | null, schedule: '*/5 * * * *', command: '', enabled: true, err: '', busy: false })
watch(() => form.preset, (v) => { if (v) form.schedule = String(v) })
onMounted(() => {
  if (typeof route.query.project === 'string') { form.project = route.query.project; form.open = true }
})
function startNew() {
  Object.assign(form, { open: true, id: '', command: '', schedule: '*/5 * * * *', preset: '*/5 * * * *', enabled: true, err: '' })
  if (form.project === '' && me.data.value?.role !== 'admin') form.project = projects.data.value?.[0]?.id ?? ''
}
function startEdit(j: CronJob) {
  Object.assign(form, { open: true, id: j.id, project: j.project_id ?? '', user: j.user, schedule: j.schedule, preset: PRESETS.some((p) => p.value === j.schedule) ? j.schedule : null, command: j.command, enabled: j.enabled, err: '' })
}
const examples = computed(() => {
  const p = (projects.data.value ?? []).find((x) => x.id === form.project)
  return p?.kind === 'laravel'
    ? ['php artisan schedule:run', 'php artisan queue:work --stop-when-empty', 'php artisan backup:run']
    : ['php cron.php', 'curl -fsS https://example.com/cron', 'find storage/tmp -mtime +7 -delete']
})
async function save() {
  form.err = ''; form.busy = true
  const body = { project_id: form.project || null, user: form.project ? undefined : form.user, schedule: form.schedule.trim(), command: form.command.trim(), enabled: form.enabled }
  try {
    if (form.id) await api.put(`/api/cron/${encodeURIComponent(form.id)}`, body)
    else await api.post('/api/cron', body)
    form.open = false
    jobs.refresh()
  } catch (e) { if (!(e instanceof SudoCancelled)) form.err = errorMessage(e) } finally { form.busy = false }
}
const runJob = ref<Record<string, string>>({})
const logs = ref<Record<string, string>>({})
const err = ref('')
async function runNow(j: CronJob) {
  err.value = ''
  try {
    const r = await api.post<{ job_id: string }>(`/api/cron/${encodeURIComponent(j.id)}/run`)
    runJob.value = { ...runJob.value, [j.id]: r.job_id }
  } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) }
}
async function showLog(j: CronJob) {
  if (logs.value[j.id] !== undefined) { const { [j.id]: _, ...rest } = logs.value; logs.value = rest; return }
  try {
    const r = await api.get<{ log: string }>(`/api/cron/${encodeURIComponent(j.id)}/log`)
    logs.value = { ...logs.value, [j.id]: r.log || '(no output yet)' }
  } catch (e) { err.value = errorMessage(e) }
}
async function toggle(j: CronJob) {
  try { await api.put(`/api/cron/${encodeURIComponent(j.id)}`, { project_id: j.project_id, user: j.user, schedule: j.schedule, command: j.command, enabled: !j.enabled }); jobs.refresh() } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) }
}
async function remove(j: CronJob) {
  try { await api.del(`/api/cron/${encodeURIComponent(j.id)}`); jobs.refresh() } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) }
}
function lastRun(j: CronJob) {
  if (!j.last_start) return { cls: '', text: 'never ran' }
  const when = formatRelative(new Date(j.last_start * 1000).toISOString())
  if (j.last_exit === null) return { cls: 'warn', text: `running since ${when}` }
  return j.last_exit === 0 ? { cls: 'ok', text: `ok · ${when}` } : { cls: 'bad', text: `exit ${j.last_exit} · ${when}` }
}
const stats = computed(() => {
  const l = jobs.data.value ?? []
  return [
    { k: 'Jobs', v: String(l.length) },
    { k: 'Enabled', v: String(l.filter((j) => j.enabled).length) },
    { k: 'Failed last run', v: String(l.filter((j) => j.last_exit !== null && j.last_exit !== 0).length) },
    { k: 'Projects with jobs', v: String(new Set(l.map((j) => j.project_id).filter(Boolean)).size) },
  ]
})
</script>

<template>
  <header style="display: flex; flex-wrap: wrap; align-items: flex-end; justify-content: space-between; gap: 16px">
    <div style="display: flex; flex-direction: column; gap: 6px">
      <p class="lbl">{{ host }} / Cron jobs</p>
      <h1 style="font-size: 30px; font-weight: 600">Cron jobs</h1>
    </div>
    <button v-if="me.data.value?.role !== 'viewer'" type="button" class="btn btn-p" @click="startNew"><AppIcon name="plus" :size="15" /> New cron job</button>
  </header>

  <section aria-label="Cron stats" style="display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 12px">
    <div v-for="s in stats" :key="s.k" class="card" style="padding: 16px; display: flex; flex-direction: column; gap: 6px">
      <span class="lbl">{{ s.k }}</span>
      <span style="font-family: 'Space Grotesk', sans-serif; font-size: 24px; font-weight: 600">{{ s.v }}</span>
    </div>
  </section>

  <section v-if="form.open" class="card" aria-labelledby="cf-h" style="padding: 20px; display: flex; flex-direction: column; gap: 14px">
    <h2 id="cf-h" style="font-size: 17px; font-weight: 600">{{ form.id ? 'Edit cron job' : 'New cron job' }}</h2>
    <div style="display: flex; flex-wrap: wrap; gap: 12px">
      <label style="flex: 1 1 240px; display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Runs for</span><SearchSelect v-model="form.project" :options="targetOpts" placeholder="Choose a project" aria-label="Project" /></label>
      <label v-if="!form.project" style="flex: 1 1 160px; display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Linux user</span><input v-model="form.user" class="in mono" type="text" placeholder="root" spellcheck="false"></label>
      <label style="flex: 1 1 240px; display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">How often</span><SearchSelect v-model="form.preset" :options="PRESETS" placeholder="Custom schedule" aria-label="How often" /></label>
      <label style="flex: 1 1 180px; display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Schedule (cron)</span><input v-model="form.schedule" class="in mono" type="text" placeholder="*/5 * * * *" spellcheck="false" @input="form.preset = null"></label>
    </div>
    <label style="display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Command</span><input v-model="form.command" class="in mono" type="text" :placeholder="examples[0]" spellcheck="false"></label>
    <div style="display: flex; flex-wrap: wrap; gap: 6px; align-items: center"><span style="font-size: 12px; color: #A3AEBD">Examples:</span><button v-for="e in examples" :key="e" type="button" class="pill mono" style="cursor: pointer; border: 0" @click="form.command = e">{{ e }}</button></div>
    <label style="display: flex; gap: 8px; align-items: center; font-size: 13px; color: #C5CEDA"><input v-model="form.enabled" type="checkbox"> Enabled</label>
    <p style="font-size: 12px; color: #A3AEBD">Output of the last run is kept, and a failing job shows up red here. Project jobs run as the project's PHP user inside its folder; Laravel's own scheduler is turned on under the project's Settings.</p>
    <p v-if="form.err" class="small" style="color: var(--bad)" role="alert">{{ form.err }}</p>
    <div style="display: flex; gap: 8px"><button type="button" class="btn btn-p" :disabled="form.busy || !form.command.trim() || !form.schedule.trim()" @click="save">{{ form.busy ? 'Saving…' : form.id ? 'Save' : 'Add cron job' }}</button><button type="button" class="btn btn-s" @click="form.open = false">Cancel</button></div>
  </section>

  <section class="card" aria-labelledby="cl-h" style="overflow: hidden">
    <div style="display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 16px 20px">
      <div style="display: flex; align-items: center; gap: 10px"><AppIcon name="clock" :size="18" /><h2 id="cl-h" style="font-size: 17px; font-weight: 600">Scheduled jobs</h2></div>
      <span v-if="err" class="small" style="color: var(--bad)" role="alert">{{ err }}</span>
    </div>
    <div style="overflow-x: auto">
      <table>
        <thead><tr><th>Schedule</th><th>Command</th><th>Project / user</th><th>Last run</th><th><span style="position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0)">Actions</span></th></tr></thead>
        <tbody>
          <template v-for="j in jobs.data.value ?? []" :key="j.id">
            <tr :style="j.enabled ? '' : 'opacity: .55'">
              <td><div style="display: flex; flex-direction: column; gap: 2px"><span style="font-size: 13px; font-weight: 500">{{ describe(j.schedule) }}</span><span class="mono" style="font-size: 12px; color: #A3AEBD">{{ j.schedule }}</span></div></td>
              <td class="mono" style="font-size: 13px; max-width: 380px; overflow: hidden; text-overflow: ellipsis" :title="j.command">{{ j.command }}</td>
              <td style="font-size: 13px; color: #C5CEDA"><NuxtLink v-if="j.project_id" :to="`/projects/${encodeURIComponent(j.project_id)}`" style="color: #C5CEDA">{{ j.project_id }}</NuxtLink><template v-else>server</template> · <span class="mono">{{ j.user }}</span></td>
              <td><span :class="`pill ${lastRun(j).cls}`">{{ j.enabled ? lastRun(j).text : 'paused' }}</span></td>
              <td>
                <div v-if="me.data.value?.role !== 'viewer'" style="display: flex; gap: 6px; flex-wrap: wrap">
                  <button type="button" class="btn btn-s" @click="runNow(j)"><AppIcon name="play" :size="13" /> Run now</button>
                  <button type="button" class="btn btn-s" :aria-expanded="logs[j.id] !== undefined" @click="showLog(j)">Output</button>
                  <button type="button" class="btn btn-s" @click="toggle(j)">{{ j.enabled ? 'Pause' : 'Resume' }}</button>
                  <button type="button" class="btn btn-s" :aria-label="`Edit ${j.command}`" @click="startEdit(j)"><AppIcon name="edit" :size="13" /></button>
                  <ConfirmAction label="Delete" confirm-label="Delete job" @confirm="remove(j)" />
                </div>
                <button v-else type="button" class="btn btn-s" @click="showLog(j)">Output</button>
              </td>
            </tr>
            <tr v-if="runJob[j.id] || logs[j.id] !== undefined">
              <td colspan="5" style="background: #0F1318; white-space: normal">
                <JobLog v-if="runJob[j.id]" :key="runJob[j.id]" :job-id="runJob[j.id]!" status="running" @finished="jobs.refresh()" />
                <pre v-else class="mono" style="margin: 0; max-height: 320px; overflow: auto; font-size: 12px; line-height: 1.6; color: #C5CEDA; white-space: pre-wrap">{{ logs[j.id] }}</pre>
              </td>
            </tr>
          </template>
          <tr v-if="!(jobs.data.value ?? []).length"><td colspan="5" style="color: #A3AEBD">{{ jobs.loading.value ? 'Loading…' : 'No cron jobs yet.' }}</td></tr>
        </tbody>
      </table>
    </div>
  </section>
</template>
