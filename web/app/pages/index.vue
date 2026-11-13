<script setup lang="ts">
// Board 01 · Dashboard — same markup as the design, real data.
import type { BackupPlan, BackupRun, Overview, SecurityInfo, Service, SystemInfo } from '~/types/api'

useHead({ title: 'Overview · NexaCloud' })
const api = useApi()
const ov = useResource<Overview>('/api/overview', { interval: 15_000 })
const sys = useResource<SystemInfo>('/api/system', { interval: 10_000 })
const hist = useResource<{ points: { ts: number; cpu: number | null; mem: number | null }[]; net: { rx_24h: number; tx_24h: number; rx_month: number; tx_month: number; points: number[] } }>('/api/system/history', { interval: 60_000 })
const ws = useResource<{ id: string; site: { kind: string; php_version: string | null }; project: { repo: string; branch: string } | null; last_deploy: { status: string; started_at: string } | null; monitor: { uptime: number | null; last_code: number | null; p95: number | null } }[]>('/api/workspace', { interval: 30_000 })
const svcs = useResource<Service[]>('/api/services', { interval: 20_000 })
const deploys = useResource<{ id: string; project_id: string; status: string; commit: string | null; message: string | null; started_at: string }[]>('/api/deploys', { interval: 20_000 })
const plans = useResource<BackupPlan[]>('/api/backup/plans')
const runs = useResource<BackupRun[]>('/api/backup/runs', { interval: 30_000 })
const dests = useResource<{ id: string; name: string; kind: string }[]>('/api/backup/destinations')
const sec = useResource<SecurityInfo>('/api/security')
const adv = useResource<{ score: number }>('/api/advisor')

const host = computed(() => ov.data.value?.hostname ?? sys.data.value?.hostname ?? 'server')
const search = ref('')
function doSearch() { navigateTo({ path: '/projects', query: search.value.trim() ? { q: search.value.trim() } : {} }) }

// attention
const alerts = computed(() => (ov.data.value?.alerts ?? []).map((a) => ({
  cls: a.level === 'critical' ? 'bad' : a.level === 'warning' ? 'warn' : 'info',
  level: a.level === 'critical' ? 'Critical' : a.level === 'warning' ? 'Warning' : 'Info',
  text: a.detail ? `${a.title} — ${a.detail}` : a.title,
  href: a.link || '/advisor',
  action: 'Review',
})))

// resources
const gb = (b: number) => b / 1024 ** 3
const mem = computed(() => sys.data.value?.memory)
const memPct = computed(() => (mem.value?.total ? Math.round((mem.value.used / mem.value.total) * 100) : 0))
const disk = computed(() => sys.data.value?.disks?.find((d) => d.mount === '/') ?? sys.data.value?.disks?.[0])
const diskPct = computed(() => (disk.value?.total ? Math.round((disk.value.used / disk.value.total) * 100) : 0))
function spark(vals: (number | null)[], max?: number) {
  const v = vals.filter((x): x is number => x !== null)
  if (!v.length) return '0,36 200,36'
  const top = max ?? Math.max(...v, 1)
  return vals.map((x, i) => x === null ? null : `${((i / (vals.length - 1)) * 200).toFixed(1)},${(38 - (x / top) * 34).toFixed(1)}`).filter(Boolean).join(' ')
}
const cpuLine = computed(() => spark((hist.data.value?.points ?? []).map((p) => p.cpu), 100))
const memLine = computed(() => spark((hist.data.value?.points ?? []).map((p) => p.mem), 100))
const netLine = computed(() => spark(hist.data.value?.net.points ?? []))
const net = computed(() => hist.data.value?.net)

// projects
const projects = computed(() => (ws.data.value ?? []).filter((c) => c.site.kind !== 'proxy').slice(0, 8).map((c) => {
  const code = c.monitor.last_code
  const failed = c.last_deploy && ['failed', 'rolled_back'].includes(c.last_deploy.status)
  const [cls, status] = code !== null && (code === 0 || code >= 500) ? ['bad', 'Down'] : failed ? ['bad', 'Deploy failed'] : code === null ? ['off', 'Not checked'] : ['ok', 'Healthy']
  return {
    id: c.id, name: c.id, repo: c.project?.repo ? `${c.project.repo.replace(/^.*[:/]([^/:]+\/[^/]+?)(\.git)?$/, '$1')} @ ${c.project.branch}` : 'No Git connected',
    fw: ({ laravel: 'Laravel', php: 'PHP', static: 'Static' } as Record<string, string>)[c.site.kind] ?? c.site.kind,
    php: c.site.php_version, cls, status,
    p95: c.monitor.p95 !== null ? `${c.monitor.p95} ms` : '—',
    uptime: c.monitor.uptime !== null ? `${(c.monitor.uptime * 100).toFixed(2)}%` : '—',
    deploy: c.last_deploy ? formatRelative(c.last_deploy.started_at) : '—',
  }
}))

// services
const services = computed(() => (svcs.data.value ?? []).filter((s) => s.installed).slice(0, 7).map((s) => ({
  unit: s.unit, name: s.label, meta: `${s.unit}${s.version ? ' · ' + s.version : ''}`,
  cls: s.active ? 'ok' : 'bad', state: s.active ? 'Running' : 'Stopped',
})))
const restarting = ref<string | null>(null)
async function restart(unit: string) {
  restarting.value = unit
  try { await api.post(`/api/services/${encodeURIComponent(unit)}/restart`); setTimeout(() => svcs.refresh(), 1500) } catch { /* sudo cancelled */ } finally { restarting.value = null }
}

// deployments
const recent = computed(() => (deploys.data.value ?? []).slice(0, 5).map((d) => ({
  project: d.project_id,
  commit: `${d.commit?.slice(0, 7) ?? '—'} · ${d.message ?? ''}`,
  when: formatRelative(d.started_at),
  cls: d.status === 'ok' ? 'ok' : ['running', 'queued'].includes(d.status) ? 'info' : 'bad',
  state: ({ ok: 'Live', running: 'Deploying', queued: 'Queued', failed: 'Failed', rolled_back: 'Rolled back' } as Record<string, string>)[d.status] ?? d.status,
})))

// backups
const lastRun = computed(() => runs.data.value?.[0] ?? null)
const plan = computed(() => plans.data.value?.find((p) => p.enabled) ?? plans.data.value?.[0] ?? null)
const offsite = computed(() => {
  const d = dests.data.value?.find((x) => x.id === plan.value?.destination_id)
  return d ? `${d.name} · ${d.kind.toUpperCase()}` : 'This server only'
})
const backingUp = ref(false)
async function backupNow() {
  if (!plan.value) return navigateTo('/backups')
  backingUp.value = true
  try { await api.post(`/api/backup/plans/${plan.value.id}/run`, {}); setTimeout(() => runs.refresh(), 2000) } catch { /* cancelled */ } finally { backingUp.value = false }
}

// security
const security = computed(() => {
  const s = sec.data.value
  if (!s) return []
  return [
    { name: 'Firewall (UFW)', cls: s.ufw.active ? 'ok' : 'bad', state: s.ufw.active ? 'Active' : 'Off' },
    { name: 'Fail2ban', cls: s.fail2ban.active ? 'ok' : 'warn', state: s.fail2ban.active ? 'Running' : s.fail2ban.installed ? 'Stopped' : 'Not installed' },
    { name: 'Automatic security updates', cls: s.unattended_upgrades ? 'ok' : 'warn', state: s.unattended_upgrades ? 'On' : 'Off' },
    { name: 'SSH password login', cls: s.ssh_password_auth ? 'warn' : 'ok', state: s.ssh_password_auth ? 'Allowed' : 'Keys only' },
  ]
})
const score = computed(() => adv.data.value?.score ?? null)
</script>

<template>
  <header style="display: flex; flex-wrap: wrap; align-items: flex-end; justify-content: space-between; gap: 16px">
    <div style="display: flex; flex-direction: column; gap: 6px">
      <p class="lbl">{{ host }} / Overview</p>
      <h1 style="font-size: 30px; font-weight: 600">Server overview</h1>
    </div>
    <div style="display: flex; flex-wrap: wrap; gap: 10px; align-items: center">
      <form style="display: flex; align-items: center; gap: 8px; min-height: 44px; padding: 0 12px; background: #12171D; border: 1px solid #222A34; border-radius: 8px; min-width: 260px; box-sizing: border-box" @submit.prevent="doSearch">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="#A3AEBD" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="11" cy="11" r="7" /><path d="M20 20l-3.5-3.5" /></svg>
        <input v-model="search" type="search" aria-label="Search projects and domains" placeholder="Search projects, domains, logs" style="flex: 1; background: transparent; border: 0; outline: none; color: #E7ECF2; font-size: 14px; min-width: 0">
        <span class="mono" style="font-size: 11px; color: #A3AEBD; border: 1px solid #2A3441; border-radius: 5px; padding: 2px 6px">↵</span>
      </form>
      <NuxtLink class="btn btn-p" to="/projects?new=1">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" aria-hidden="true"><path d="M12 5v14M5 12h14" /></svg>
        New project
      </NuxtLink>
    </div>
  </header>

  <section v-if="alerts.length" class="card" aria-labelledby="attn" style="padding: 18px 20px; border-color: #4A3B1C; background: #17140D; display: flex; flex-direction: column; gap: 12px">
    <div style="display: flex; align-items: center; gap: 10px">
      <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="#F7C55E" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3l10 18H2L12 3zM12 10v5M12 18v.01" /></svg>
      <h2 id="attn" style="font-size: 16px; font-weight: 600">{{ alerts.length }} {{ alerts.length === 1 ? 'thing needs' : 'things need' }} your attention</h2>
    </div>
    <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(280px, 1fr)); gap: 10px">
      <div v-for="(a, aI) in alerts" :key="aI" style="display: flex; align-items: center; gap: 12px; padding: 12px 14px; background: #12171D; border: 1px solid #222A34; border-radius: 10px">
        <span :class="`pill ${a.cls}`">{{ a.level }}</span>
        <span style="flex: 1; font-size: 13px; line-height: 1.4; white-space: normal">{{ a.text }}</span>
        <NuxtLink class="btn btn-s" :to="a.href">{{ a.action }}</NuxtLink>
      </div>
    </div>
  </section>

  <section aria-label="Server resources" style="display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 16px">
    <div class="card" style="padding: 18px; display: flex; flex-direction: column; gap: 10px">
      <p class="lbl">CPU · {{ sys.data.value?.cpu_count ?? '—' }} vCPU</p>
      <p style="font-family: 'Space Grotesk', sans-serif; font-size: 32px; font-weight: 600">{{ sys.data.value ? Math.round(sys.data.value.cpu_percent) : '—' }}<span style="font-size: 16px; color: #A3AEBD">%</span></p>
      <svg viewBox="0 0 200 40" style="width: 100%; height: 40px" aria-hidden="true"><polyline :points="cpuLine" fill="none" stroke="#3DDC97" stroke-width="2" /></svg>
      <p class="lbl">Last 24 hours · load {{ sys.data.value?.load?.[0]?.toFixed(2) ?? '—' }}</p>
    </div>
    <div class="card" style="padding: 18px; display: flex; flex-direction: column; gap: 10px">
      <p class="lbl">Memory · {{ mem ? Math.round(gb(mem.total)) : '—' }} GB</p>
      <p style="font-family: 'Space Grotesk', sans-serif; font-size: 32px; font-weight: 600">{{ mem ? memPct : '—' }}<span style="font-size: 16px; color: #A3AEBD">%</span></p>
      <svg viewBox="0 0 200 40" style="width: 100%; height: 40px" aria-hidden="true"><polyline :points="memLine" fill="none" stroke="#6CB4FF" stroke-width="2" /></svg>
      <p class="lbl">≈ {{ mem ? gb(mem.used).toFixed(1) : '—' }} GB used</p>
    </div>
    <div class="card" style="padding: 18px; display: flex; flex-direction: column; gap: 10px">
      <p class="lbl">Disk</p>
      <p style="font-family: 'Space Grotesk', sans-serif; font-size: 32px; font-weight: 600">{{ disk ? Math.round(gb(disk.used)) : '—' }}<span style="font-size: 16px; color: #A3AEBD"> / {{ disk ? Math.round(gb(disk.total)) : '—' }} GB</span></p>
      <div style="height: 8px; border-radius: 99px; background: #1F2731; overflow: hidden; margin: 16px 0" role="img" :aria-label="`${diskPct} percent used`"><div :style="`width: ${diskPct}%; height: 100%; background: ${diskPct > 85 ? '#FF7A7A' : diskPct > 70 ? '#F5B83D' : '#3DDC97'}`" /></div>
      <NuxtLink to="/backups" style="font-size: 12px">See what uses the space</NuxtLink>
    </div>
    <div class="card" style="padding: 18px; display: flex; flex-direction: column; gap: 10px">
      <p class="lbl">Bandwidth · this month</p>
      <p style="font-family: 'Space Grotesk', sans-serif; font-size: 32px; font-weight: 600">{{ net ? formatBytes(net.rx_month + net.tx_month).split(' ')[0] : '—' }}<span style="font-size: 16px; color: #A3AEBD"> {{ net ? formatBytes(net.rx_month + net.tx_month).split(' ')[1] : '' }}</span></p>
      <svg viewBox="0 0 200 40" style="width: 100%; height: 40px" aria-hidden="true"><polyline :points="netLine" fill="none" stroke="#B79CFF" stroke-width="2" /></svg>
      <p class="lbl">In {{ net ? formatBytes(net.rx_24h) : '—' }} · Out {{ net ? formatBytes(net.tx_24h) : '—' }} (24 h)</p>
    </div>
  </section>

  <div style="display: flex; flex-wrap: wrap; gap: 16px; align-items: flex-start">
    <section class="card" aria-labelledby="proj" style="flex: 2 1 640px; min-width: 0; overflow: hidden">
      <div style="display: flex; align-items: center; justify-content: space-between; padding: 16px 20px; gap: 12px; flex-wrap: wrap">
        <h2 id="proj" style="font-size: 17px; font-weight: 600">Project health</h2>
        <NuxtLink to="/projects" style="font-size: 13px">All projects</NuxtLink>
      </div>
      <div style="overflow-x: auto">
        <table>
          <thead><tr><th>Project</th><th>Stack</th><th>Status</th><th>p95</th><th>Uptime 24 h</th><th>Last deploy</th><th><span style="position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0)">Open</span></th></tr></thead>
          <tbody>
            <tr v-for="p in projects" :key="p.id">
              <td><div style="display: flex; flex-direction: column; gap: 3px"><span style="font-weight: 600">{{ p.name }}</span><span style="font-size: 12px; color: #A3AEBD">{{ p.repo }}</span></div></td>
              <td><div style="display: flex; gap: 6px"><span class="tag">{{ p.fw }}</span><span v-if="p.php" class="tag mono">PHP {{ p.php }}</span></div></td>
              <td><span :class="`pill ${p.cls}`">{{ p.status }}</span></td>
              <td class="mono" style="font-size: 13px">{{ p.p95 }}</td>
              <td class="mono" style="font-size: 13px">{{ p.uptime }}</td>
              <td style="font-size: 13px; color: #C5CEDA">{{ p.deploy }}</td>
              <td><NuxtLink class="btn btn-s" :to="`/projects/${encodeURIComponent(p.id)}`">Open</NuxtLink></td>
            </tr>
            <tr v-if="!projects.length"><td colspan="7" style="color: #A3AEBD; white-space: normal">No projects yet. <NuxtLink to="/projects?new=1">Create one</NuxtLink> or <NuxtLink to="/install">import the sites already running</NuxtLink>.</td></tr>
          </tbody>
        </table>
      </div>
    </section>

    <section class="card" aria-labelledby="svc" style="flex: 1 1 320px; min-width: 0; padding: 16px 20px; display: flex; flex-direction: column; gap: 4px">
      <div style="display: flex; align-items: center; justify-content: space-between; margin-bottom: 8px">
        <h2 id="svc" style="font-size: 17px; font-weight: 600">Services</h2>
        <NuxtLink to="/php" style="font-size: 13px">Manage</NuxtLink>
      </div>
      <div v-for="s in services" :key="s.unit" style="display: flex; align-items: center; gap: 12px; padding: 10px 0; border-bottom: 1px solid #1B222B">
        <div style="flex: 1; display: flex; flex-direction: column; gap: 2px; min-width: 0">
          <span style="font-size: 14px; font-weight: 600">{{ s.name }}</span>
          <span class="mono" style="font-size: 12px; color: #A3AEBD; overflow: hidden; text-overflow: ellipsis; white-space: nowrap">{{ s.meta }}</span>
        </div>
        <span :class="`pill ${s.cls}`">{{ s.state }}</span>
        <button type="button" :aria-label="`Restart ${s.name}`" :disabled="restarting === s.unit" style="width: 36px; height: 36px; border-radius: 8px; border: 1px solid #2A3441; background: #171D25; color: #C5CEDA; cursor: pointer; display: grid; place-items: center" @click="restart(s.unit)">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M21 12a9 9 0 11-3-6.7L21 8M21 3v5h-5" /></svg>
        </button>
      </div>
    </section>
  </div>

  <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr)); gap: 16px">
    <section class="card" aria-labelledby="dep" style="padding: 16px 20px; display: flex; flex-direction: column; gap: 4px">
      <div style="display: flex; align-items: center; justify-content: space-between; margin-bottom: 8px">
        <h2 id="dep" style="font-size: 17px; font-weight: 600">Recent deployments</h2>
        <NuxtLink to="/deployments" style="font-size: 13px">CI/CD</NuxtLink>
      </div>
      <div v-for="(d, dI) in recent" :key="dI" style="display: flex; align-items: center; gap: 12px; padding: 10px 0; border-bottom: 1px solid #1B222B">
        <span :class="`pill ${d.cls}`">{{ d.state }}</span>
        <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px">
          <span style="font-size: 14px; font-weight: 600">{{ d.project }}</span>
          <span class="mono" style="font-size: 12px; color: #A3AEBD; overflow: hidden; text-overflow: ellipsis; white-space: nowrap">{{ d.commit }}</span>
        </div>
        <span style="font-size: 12px; color: #A3AEBD">{{ d.when }}</span>
      </div>
      <p v-if="!recent.length" style="font-size: 13px; color: #A3AEBD; padding: 10px 0">No deployments yet.</p>
    </section>

    <section class="card" aria-labelledby="bk" style="padding: 16px 20px; display: flex; flex-direction: column; gap: 14px">
      <div style="display: flex; align-items: center; justify-content: space-between">
        <h2 id="bk" style="font-size: 17px; font-weight: 600">Backups</h2>
        <NuxtLink to="/backups" style="font-size: 13px">Manage</NuxtLink>
      </div>
      <div style="display: flex; align-items: center; gap: 12px; flex-wrap: wrap">
        <span :class="`pill ${!lastRun ? 'off' : lastRun.status === 'ok' ? 'ok' : lastRun.status === 'running' ? 'info' : 'bad'}`">{{ !lastRun ? 'No backup yet' : lastRun.status === 'ok' ? 'Last run OK' : lastRun.status === 'running' ? 'Running' : 'Last run failed' }}</span>
        <span v-if="lastRun" style="font-size: 13px; color: #C5CEDA">{{ formatRelative(lastRun.started_at) }} · {{ lastRun.items.filter((i) => i.kind === 'database').length }} databases · {{ formatBytes(lastRun.size_bytes) }}</span>
      </div>
      <div style="display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px">
        <div style="padding: 12px; background: #151B22; border-radius: 10px; display: flex; flex-direction: column; gap: 4px"><span class="lbl">Schedule</span><span style="font-size: 15px; font-weight: 600">{{ plan ? plan.schedule : 'Not set up' }}</span></div>
        <div style="padding: 12px; background: #151B22; border-radius: 10px; display: flex; flex-direction: column; gap: 4px"><span class="lbl">Retention</span><span style="font-size: 15px; font-weight: 600">{{ plan ? `${plan.keep_daily} daily · ${plan.keep_weekly} weekly` : '—' }}</span></div>
        <div style="padding: 12px; background: #151B22; border-radius: 10px; display: flex; flex-direction: column; gap: 4px"><span class="lbl">Off-site copy</span><span style="font-size: 15px; font-weight: 600">{{ plan ? offsite : '—' }}</span></div>
        <div style="padding: 12px; background: #151B22; border-radius: 10px; display: flex; flex-direction: column; gap: 4px"><span class="lbl">Backup jobs</span><span style="font-size: 15px; font-weight: 600">{{ plans.data.value?.length ?? 0 }} · {{ (plans.data.value ?? []).filter((p) => p.enabled).length }} active</span></div>
      </div>
      <button type="button" class="btn" style="align-self: flex-start" :disabled="backingUp" @click="backupNow">{{ plan ? (backingUp ? 'Starting…' : 'Back up now') : 'Set up backups' }}</button>
    </section>

    <section class="card" aria-labelledby="sec" style="padding: 16px 20px; display: flex; flex-direction: column; gap: 4px">
      <div style="display: flex; align-items: center; justify-content: space-between; margin-bottom: 8px">
        <h2 id="sec" style="font-size: 17px; font-weight: 600">Security</h2>
        <span v-if="score !== null" :class="`pill ${score >= 80 ? 'ok' : score >= 50 ? 'warn' : 'bad'}`">Score {{ score }} / 100</span>
      </div>
      <div v-for="(s, sI) in security" :key="sI" style="display: flex; align-items: center; gap: 12px; padding: 10px 0; border-bottom: 1px solid #1B222B">
        <span style="flex: 1; font-size: 14px">{{ s.name }}</span>
        <span :class="`pill ${s.cls}`">{{ s.state }}</span>
      </div>
      <NuxtLink class="btn btn-p" to="/advisor" style="align-self: flex-start; margin-top: 10px">Apply recommended hardening</NuxtLink>
    </section>
  </div>
</template>
