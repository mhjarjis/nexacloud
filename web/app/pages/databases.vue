<script setup lang="ts">
// Board 07 · Databases — same markup as the design, real data.
import type { Backup, DatabasesInfo, Engine } from '~/types/api'

useHead({ title: 'Databases · NexaCloud' })
const api = useApi()
const dbs = useResource<DatabasesInfo>('/api/databases', { interval: 30_000 })
const backups = useResource<Backup[]>('/api/backups', { interval: 30_000 })
const projects = useResource<{ id: string }[]>('/api/projects')
const sys = useResource<{ hostname: string }>('/api/system')
const host = computed(() => sys.data.value?.hostname ?? 'server')

const eng = ref<Engine>('mysql')
watch(() => dbs.data.value, (d) => { if (d && !d.mysql.available && d.postgres.available) eng.value = 'postgres' }, { immediate: true })
const engines = computed(() => (['mysql', 'postgres'] as Engine[]).map((id) => {
  const e = dbs.data.value?.[id]
  return { id, label: `${id === 'mysql' ? 'MySQL' : 'PostgreSQL'}${e?.version ? ' ' + e.version.split('.').slice(0, 2).join('.') : ''}`, available: !!e?.available, pressed: eng.value === id, bg: eng.value === id ? '#1F2833' : 'transparent', fg: eng.value === id ? '#E7ECF2' : '#A3AEBD' }
}))
const cur = computed(() => dbs.data.value?.[eng.value] ?? null)

interface Stats { connections?: number; max_connections?: number; hit_rate?: number; tables?: Record<string, number>; users?: Record<string, number>; owners?: Record<string, string>; slow?: { sql: string; db: string; secs: string; count: string }[]; slow_unavailable?: string; used_by: Record<string, string>; local_only?: boolean; listen?: string[] }
const stats = ref<Stats | null>(null)
async function loadStats(retry = true) {
  try { stats.value = await api.get<Stats>(`/api/dbm/${eng.value}/stats`) } catch { if (retry) setTimeout(() => loadStats(false), 2500) }
}
watch(eng, () => { stats.value = null; loadStats() }, { immediate: true })
let statsTimer: ReturnType<typeof setInterval> | null = null
onMounted(() => { statsTimer = setInterval(() => loadStats(false), 30_000) })
onBeforeUnmount(() => { if (statsTimer) clearInterval(statsTimer) })
const users = ref<{ user: string; host: string; databases: string }[]>([])
async function loadUsers() {
  users.value = []
  try {
    const raw = await api.get<{ user: string; host: string; databases: string }[]>(`/api/dbm/${eng.value}/users`)
    // one row per user (MySQL keeps one account per host)
    const by = new Map<string, { user: string; host: string; databases: string }>()
    for (const u of raw) {
      const e = by.get(u.user)
      if (e) { e.host = [...new Set([...e.host.split(', '), u.host])].join(', '); if (!e.databases) e.databases = u.databases } else by.set(u.user, { ...u })
    }
    users.value = [...by.values()]
  } catch { users.value = [] }
}
watch(eng, loadUsers, { immediate: true })

const statCards = computed(() => {
  const list = cur.value?.databases ?? []
  const s = stats.value
  return [
    { k: 'Databases', v: String(list.length) },
    { k: 'Total size', v: formatBytes(list.reduce((n, d) => n + d.size_bytes, 0)) },
    { k: 'Connections now', v: s?.connections != null ? `${Math.round(s.connections)} / ${s.max_connections ?? '—'}` : '—' },
    { k: eng.value === 'mysql' ? 'Buffer pool hit rate' : 'Cache hit rate', v: s?.hit_rate != null ? `${s.hit_rate}%` : '—' },
  ]
})
function lastBackup(name: string) { return (backups.data.value ?? []).find((b) => b.engine === eng.value && b.database === name) ?? null }
const rows = computed(() => (cur.value?.databases ?? []).map((d) => {
  const b = lastBackup(d.name)
  const age = b ? (Date.now() - new Date(b.created_at).getTime()) / 3600_000 : null
  return {
    name: d.name,
    project: stats.value?.used_by[d.name] ?? '—',
    size: formatBytes(d.size_bytes),
    tables: String(stats.value?.tables?.[d.name] ?? d.tables ?? '—'),
    users: eng.value === 'mysql' ? String(stats.value?.users?.[d.name] ?? 0) : (stats.value?.owners?.[d.name] ?? '—'),
    backup: b ? formatRelative(b.created_at) : 'Never',
    cls: age === null ? 'warn' : age < 30 ? 'ok' : 'warn',
    file: b?.file ?? null,
  }
}))
const q = ref('')
const shownRows = computed(() => rows.value.filter((r) => !q.value.trim() || `${r.name} ${r.project}`.toLowerCase().includes(q.value.trim().toLowerCase())))
const more = ref<string | null>(null)
const busy = ref<Record<string, string>>({})
const backupJob = ref<Record<string, string>>({})
async function backUp(name: string) {
  busy.value[name] = 'Backing up…'
  try { backupJob.value[name] = (await api.post<{ job_id: string }>(`/api/databases/${eng.value}/${encodeURIComponent(name)}/backup`)).job_id; busy.value[name] = 'Backing up…' } catch (e) { busy.value[name] = e instanceof SudoCancelled ? '' : errorMessage(e) }
}
function backupDone(name: string, status: string) { busy.value[name] = status === 'ok' ? 'Done — backup saved.' : 'Backup failed — see the output.'; backups.refresh() }

// ---- create database
const form = reactive({ name: '', user: '', connect: '' })
watch(() => form.name, (n, old) => { if (!form.user || form.user === `${old}_user`.slice(0, 32)) form.user = n ? `${n}_user`.slice(0, 32) : '' })
const projectOptions = computed(() => [{ value: '', label: "Don't connect" }, ...(projects.data.value ?? []).map((p) => ({ value: p.id, label: p.id }))])
const nameOk = computed(() => /^[a-zA-Z0-9_]{1,63}$/.test(form.name) && /^[a-zA-Z0-9_]{1,32}$/.test(form.user))
const created = ref<{ database: string; user: string; password: string; env: string } | null>(null)
const createErr = ref<string | null>(null)
const creating = ref(false)
const connectState = ref<string | null>(null)
async function createDb() {
  creating.value = true
  createErr.value = null
  connectState.value = null
  try {
    created.value = await api.post(`/api/dbm/${eng.value}/databases`, { name: form.name, user: form.user })
    dbs.refresh(); loadUsers(); loadStats()
    if (form.connect && created.value) {
      connectState.value = 'Writing to .env…'
      const set = Object.fromEntries(created.value.env.split('\n').map((l) => l.split('=') as [string, string]).map(([k, ...v]) => [k, v.join('=')]))
      const req = { kind: 'project_env', params: { id: form.connect, set } }
      const pv = await api.post<{ plan_hash: string }>('/api/plans/preview', req)
      const st = await api.post<{ change_id: string }>('/api/plans/apply', { ...req, plan_hash: pv.plan_hash })
      let status = 'running'
      for (let i = 0; i < 60 && !['applied', 'reverted', 'failed', 'revert_failed'].includes(status); i++) {
        await new Promise((r) => setTimeout(r, 1000))
        status = (await api.get<{ status: string }>(`/api/changes/${st.change_id}`)).status
      }
      connectState.value = status === 'applied' ? `Written to ${form.connect}'s .env (DB_DATABASE, DB_USERNAME, DB_PASSWORD).` : `Could not update the .env (${status}) — see Activity.`
      loadStats()
    }
  } catch (e) { if (!(e instanceof SudoCancelled)) createErr.value = errorMessage(e) } finally { creating.value = false }
}
async function copy(t: string) { await navigator.clipboard?.writeText(t) }

// ---- users
const resetFor = ref<Record<string, string>>({})
async function resetPw(u: { user: string; host: string }) {
  try { const r = await api.post<{ password: string }>(`/api/dbm/${eng.value}/users/password`, { user: u.user, host: u.host.includes('localhost') ? 'localhost' : (u.host.split(', ')[0] || 'localhost') }); resetFor.value[u.user] = r.password } catch (e) { if (!(e instanceof SudoCancelled)) resetFor.value[u.user] = `Error: ${errorMessage(e)}` }
}
const ipv4 = ref('')
onMounted(async () => { try { const d = await api.get<{ server_ips: string[] }>('/api/domains'); ipv4.value = d.server_ips.find((ip) => ip.includes('.') && !ip.startsWith('127.') && !ip.startsWith('172.1')) ?? '' } catch { /* ignore */ } })
const tunnel = computed(() => `SSH host: ${ipv4.value || '<server ip>'}\nSSH user: root (or your sudo user)\nDatabase host: 127.0.0.1\nPort: ${eng.value === 'mysql' ? 3306 : 5432}\n\nTerminal: ssh -L ${eng.value === 'mysql' ? '3306:127.0.0.1:3306' : '5432:127.0.0.1:5432'} root@${ipv4.value || '<server ip>'}`)
const tunnelCopied = ref(false)
</script>

<template>
  <header style="display: flex; flex-wrap: wrap; align-items: flex-end; justify-content: space-between; gap: 16px">
    <div style="display: flex; flex-direction: column; gap: 6px">
      <p class="lbl">{{ host }} / Databases</p>
      <h1 style="font-size: 30px; font-weight: 600">Databases</h1>
    </div>
    <div role="group" aria-label="Engine" style="display: flex; gap: 4px; padding: 4px; background: #12171D; border: 1px solid #222A34; border-radius: 10px">
      <button v-for="e in engines" :key="e.id" type="button" :aria-pressed="e.pressed" :disabled="!e.available" :title="e.available ? undefined : 'Not installed — install it under PHP & Services'" :style="`min-height: 36px; padding: 0 16px; border-radius: 7px; border: 0; cursor: pointer; font-size: 13px; font-weight: 600; background: ${e.bg}; color: ${e.fg}; ${e.available ? '' : 'opacity: .45;'}`" @click="eng = e.id">{{ e.label }}</button>
    </div>
  </header>

  <section v-if="cur && !cur.available" class="card" style="padding: 18px 20px; display: flex; flex-wrap: wrap; gap: 12px; align-items: center">
    <span style="flex: 1; font-size: 14px">{{ eng === 'mysql' ? 'MySQL' : 'PostgreSQL' }} is not installed on this server{{ cur.error ? ` (${cur.error})` : '' }}.</span>
    <NuxtLink class="btn btn-p" to="/php#services">Install from PHP &amp; Services</NuxtLink>
  </section>

  <section aria-label="Engine stats" style="display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 12px">
    <div v-for="s in statCards" :key="s.k" class="card" style="padding: 16px; display: flex; flex-direction: column; gap: 6px">
      <span class="lbl">{{ s.k }}</span>
      <span style="font-family: 'Space Grotesk', sans-serif; font-size: 24px; font-weight: 600">{{ s.v }}</span>
    </div>
  </section>

  <section class="card" aria-labelledby="dbl" style="overflow: hidden">
    <div style="display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px; padding: 16px 20px">
      <h2 id="dbl" style="font-size: 17px; font-weight: 600">{{ eng === 'mysql' ? 'MySQL' : 'PostgreSQL' }} {{ cur?.version?.split('.').slice(0, 2).join('.') ?? '' }} · {{ rows.length }} {{ rows.length === 1 ? 'database' : 'databases' }}</h2>
      <div style="display: flex; gap: 8px; flex-wrap: wrap">
        <input v-model="q" class="in" type="search" placeholder="Search databases or projects" aria-label="Search databases" style="width: 240px">
        <NuxtLink v-if="rows.length" class="btn btn-s" :to="`/db/${eng}/${encodeURIComponent(rows[0]!.name)}?console=1`">Query console</NuxtLink>
      </div>
    </div>
    <div style="overflow-x: auto">
      <table>
        <thead><tr><th>Database</th><th>Used by</th><th>Size</th><th>Tables</th><th>{{ eng === 'mysql' ? 'Users' : 'Owner' }}</th><th>Last backup</th><th><span style="position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0)">Actions</span></th></tr></thead>
        <tbody>
          <template v-for="d in shownRows" :key="d.name">
            <tr>
              <td class="mono" style="font-weight: 500"><NuxtLink :to="`/db/${eng}/${encodeURIComponent(d.name)}`" style="color: #E7ECF2">{{ d.name }}</NuxtLink></td>
              <td style="font-size: 13px; color: #C5CEDA"><NuxtLink v-if="d.project !== '—'" :to="`/projects/${encodeURIComponent(d.project)}`" style="color: #C5CEDA">{{ d.project }}</NuxtLink><template v-else>—</template></td>
              <td class="mono" style="font-size: 13px">{{ d.size }}</td>
              <td class="mono" style="font-size: 13px">{{ d.tables }}</td>
              <td class="mono" style="font-size: 13px">{{ d.users }}</td>
              <td><span :class="`pill ${d.cls}`">{{ d.backup }}</span></td>
              <td><div style="display: flex; gap: 6px"><button type="button" class="btn btn-s" :disabled="!!busy[d.name] && busy[d.name]!.endsWith('…')" @click="backUp(d.name)">Back up</button><NuxtLink class="btn btn-s" :to="`/db/${eng}/${encodeURIComponent(d.name)}`">Browse</NuxtLink><button type="button" class="btn btn-s" :aria-label="`More actions for ${d.name}`" :aria-expanded="more === d.name" @click="more = more === d.name ? null : d.name">···</button></div></td>
            </tr>
            <tr v-if="more === d.name || busy[d.name] || backupJob[d.name]">
              <td colspan="7" style="background: #0F1318; white-space: normal">
                <div style="display: flex; flex-wrap: wrap; gap: 8px; align-items: center">
                  <span v-if="busy[d.name]" style="font-size: 13px; color: #C5CEDA">{{ busy[d.name] }}</span>
                  <JobLog v-if="backupJob[d.name]" :key="backupJob[d.name]" :job-id="backupJob[d.name]!" status="running" style="width: 100%" @finished="(st) => backupDone(d.name, st)" />
                  <template v-if="more === d.name">
                    <a v-if="d.file" class="btn btn-s" :href="`/api/backups/download?file=${encodeURIComponent(d.file)}`">Download last backup</a>
                    <NuxtLink class="btn btn-s" to="/backups">Restore…</NuxtLink>
                    <NuxtLink class="btn btn-s" :to="`/db/${eng}/${encodeURIComponent(d.name)}?console=1`">SQL console</NuxtLink>
                  </template>
                </div>
              </td>
            </tr>
          </template>
          <tr v-if="!shownRows.length"><td colspan="7" style="color: #A3AEBD">{{ rows.length ? 'No databases match.' : 'No databases yet — create one below.' }}</td></tr>
        </tbody>
      </table>
    </div>
  </section>

  <div style="display: flex; flex-wrap: wrap; gap: 16px; align-items: flex-start">
    <section class="card" aria-labelledby="cdb" style="flex: 1 1 340px; min-width: 0; padding: 20px; display: flex; flex-direction: column; gap: 14px">
      <h2 id="cdb" style="font-size: 17px; font-weight: 600">Create database</h2>
      <template v-if="!created">
        <label style="display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Name</span><input v-model="form.name" class="in mono" type="text" placeholder="blog_prod" autocomplete="off" spellcheck="false"></label>
        <label style="display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">User</span><input v-model="form.user" class="in mono" type="text" placeholder="blog_user" autocomplete="off" spellcheck="false"></label>
        <div style="display: flex; flex-direction: column; gap: 6px">
          <span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Password</span>
          <input class="in mono" type="text" aria-label="Generated password" placeholder="24 random characters, generated on the server" readonly>
          <span style="font-size: 12px; color: #A3AEBD">Shown once after creating — copy it or connect a project below.</span>
        </div>
        <label style="display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Connect to project</span><SearchSelect v-model="form.connect" :options="projectOptions" placeholder="Don't connect" aria-label="Connect to project" /></label>
        <p style="font-size: 12px; color: #A3AEBD">When connected, DB_CONNECTION, DB_HOST, DB_PORT, DB_DATABASE, DB_USERNAME and DB_PASSWORD are written to that project's .env (with the usual check and automatic undo).</p>
        <p v-if="createErr" class="small" style="color: var(--bad)" role="alert">{{ createErr }}</p>
        <button type="button" class="btn btn-p" :disabled="!nameOk || creating || !cur?.available" @click="createDb">{{ creating ? 'Creating…' : 'Create database' }}</button>
      </template>
      <template v-else>
        <span class="pill ok" style="align-self: flex-start">Created {{ created.database }}</span>
        <div style="display: flex; flex-direction: column; gap: 6px">
          <span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Password for {{ created.user }}</span>
          <div style="display: flex; gap: 8px"><input class="in mono" type="text" :value="created.password" readonly aria-label="New password" style="flex: 1"><button type="button" class="btn btn-s" @click="copy(created.password)">Copy</button></div>
        </div>
        <div class="mono" style="background: #07090C; border: 1px solid #1B222B; border-radius: 10px; padding: 12px 14px; font-size: 12px; line-height: 1.7; color: #C5CEDA; white-space: pre-wrap">{{ created.env }}</div>
        <p v-if="connectState" style="font-size: 13px; color: #C5CEDA">{{ connectState }}</p>
        <div style="display: flex; gap: 8px"><button type="button" class="btn btn-s" @click="copy(created.env)">Copy .env lines</button><button type="button" class="btn btn-s" @click="created = null; form.name = ''; form.user = ''; form.connect = ''">Create another</button></div>
      </template>
    </section>

    <section style="flex: 2 1 520px; min-width: 0; display: flex; flex-direction: column; gap: 16px">
      <div class="card" style="overflow: hidden">
        <div style="padding: 16px 20px; display: flex; align-items: center; justify-content: space-between"><h2 style="font-size: 17px; font-weight: 600">Users</h2></div>
        <div style="overflow-x: auto">
          <table>
            <thead><tr><th>User</th><th>Host</th><th>Access</th><th>Privileges</th><th><span style="position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0)">Actions</span></th></tr></thead>
            <tbody>
              <tr v-for="u in users" :key="u.user + u.host">
                <td class="mono">{{ u.user }}</td>
                <td class="mono" style="font-size: 13px; color: #C5CEDA">{{ eng === 'mysql' ? u.host : 'local' }}</td>
                <td style="font-size: 13px; white-space: normal">{{ u.databases || '—' }}</td>
                <td><span class="tag">{{ eng === 'postgres' ? (u.host === 'superuser' ? 'Superuser' : 'Owner') : u.databases ? 'All on own DB' : 'No database' }}</span></td>
                <td><span v-if="resetFor[u.user]" class="mono" style="font-size: 12px">{{ resetFor[u.user] }}</span><button v-else type="button" class="btn btn-s" @click="resetPw(u)">Reset password</button></td>
              </tr>
              <tr v-if="!users.length"><td colspan="5" style="color: #A3AEBD">No users.</td></tr>
            </tbody>
          </table>
        </div>
      </div>
      <div class="card" style="padding: 18px 20px; display: flex; flex-direction: column; gap: 10px">
        <div style="display: flex; align-items: center; justify-content: space-between"><h2 style="font-size: 17px; font-weight: 600">Slow queries · 24 h</h2><span :class="`pill ${stats?.slow?.length ? 'warn' : 'ok'}`">{{ stats?.slow?.length ? `${stats.slow.length} over 1 s` : stats?.slow_unavailable ? 'Not tracked' : 'None over 1 s' }}</span></div>
        <div v-for="(s, i) in stats?.slow ?? []" :key="i" style="padding: 10px 12px; background: #0F1318; border: 1px solid #222A34; border-radius: 10px; display: flex; flex-wrap: wrap; gap: 8px 14px; align-items: center">
          <span class="mono" style="flex: 1 1 320px; font-size: 12px; white-space: normal; word-break: break-word">{{ s.sql }}</span>
          <span class="tag mono">{{ s.db }}</span><span class="mono" style="font-size: 12px; color: #F7C55E">{{ s.secs }} s</span><span style="font-size: 12px; color: #A3AEBD">{{ s.count }}×</span>
        </div>
        <p v-if="stats && !stats.slow?.length" style="font-size: 13px; color: #A3AEBD">{{ stats.slow_unavailable ? `Slow query tracking: ${stats.slow_unavailable}.` : 'No query averaged more than 1 second in the last 24 hours.' }}</p>
      </div>
      <div class="card" style="padding: 18px 20px; display: flex; flex-wrap: wrap; gap: 12px; align-items: center">
        <div style="flex: 1 1 300px; display: flex; flex-direction: column; gap: 4px">
          <span style="font-size: 15px; font-weight: 600">Remote access</span>
          <span style="font-size: 13px; color: #A3AEBD">{{ stats?.local_only === false ? `Listening on ${stats.listen?.join(', ')} — reachable from outside unless the firewall blocks it.` : 'Closed to the internet. Connect from TablePlus or DBeaver over an SSH tunnel instead.' }}</span>
        </div>
        <span :class="`pill ${stats?.local_only === false ? 'warn' : 'ok'}`">{{ stats?.local_only === false ? 'Open' : 'Local only' }}</span>
        <button type="button" class="btn btn-s" @click="copy(tunnel); tunnelCopied = true">{{ tunnelCopied ? 'Copied' : 'Copy SSH tunnel settings' }}</button>
      </div>
    </section>
  </div>
</template>

<style scoped>
.in { min-height: 44px; padding: 0 12px; box-sizing: border-box; width: 100%; }
</style>
