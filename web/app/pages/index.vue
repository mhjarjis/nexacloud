<script setup lang="ts">
import type { Overview, Service, Site, SystemInfo } from '~/types/api'

const REFRESH_MS = 10_000
const route = useRoute()

const overview = useResource<Overview>('/api/overview', { interval: REFRESH_MS })
const system = useResource<SystemInfo>('/api/system', { interval: REFRESH_MS })
const services = useResource<Service[]>('/api/services', { interval: REFRESH_MS })
const sites = useResource<Site[]>('/api/sites', { interval: REFRESH_MS })

const hostname = computed(() => overview.data.value?.hostname ?? system.data.value?.hostname ?? 'Server')
useHead({ title: () => `${hostname.value} · NexaCloud` })

const alerts = computed(() => overview.data.value?.alerts ?? [])
const hasCritical = computed(() => alerts.value.some((a) => a.level === 'critical'))

const refreshing = computed(
  () => overview.refreshing.value || system.refreshing.value || services.refreshing.value || sites.refreshing.value,
)

const sys = computed(() => system.data.value)
const memPct = computed(() => (sys.value ? percent(sys.value.memory.used, sys.value.memory.total) : 0))
const memLine = computed(() => {
  const m = sys.value
  if (!m) return ''
  const base = `${formatBytes(m.memory.used)} of ${formatBytes(m.memory.total)} used`
  return m.swap.total ? `${base} · swap ${formatBytes(m.swap.used)} of ${formatBytes(m.swap.total)}` : base
})
const rootDisk = computed(() => {
  const disks = sys.value?.disks ?? []
  return disks.find((d) => d.mount === '/') ?? disks[0] ?? null
})
const diskPct = computed(() => (rootDisk.value ? percent(rootDisk.value.used, rootDisk.value.total) : 0))
const diskUsed = computed(() => {
  const d = rootDisk.value
  if (!d) return { num: '—', rest: '' }
  const [num = '0', unit = ''] = formatBytes(d.used, 0).split(' ')
  return { num, rest: `\u00A0${unit} / ${formatBytes(d.total, 0)}` }
})
const otherDisks = computed(() => (sys.value?.disks ?? []).filter((d) => d !== rootDisk.value))
const diskLine = computed(() => {
  const d = rootDisk.value
  if (!d) return ''
  const n = otherDisks.value.length
  const base = `${Math.round(diskPct.value)}% used · ${formatBytes(Math.max(0, d.total - d.used))} free`
  return n ? `${base} · +${n} more ${n === 1 ? 'mount' : 'mounts'}` : base
})

function barTone(pct: number): string {
  if (pct >= 95) return 'bad'
  if (pct > 85) return 'warn'
  return ''
}

const loadText = computed(() => {
  const l = sys.value?.load ?? []
  return l.length ? l.slice(0, 3).map((n) => n.toFixed(2)).join(' · ') : '—'
})

const installedServices = computed(() => (services.data.value ?? []).filter((s) => s.installed))
const missingServices = computed(() => (services.data.value ?? []).filter((s) => !s.installed).length)

function gitRef(site: Site): string | null {
  const g = site.git
  if (!g || (!g.branch && !g.commit)) return null
  return [g.branch, g.commit].filter(Boolean).join('@')
}

function retryAll() {
  overview.refresh()
  system.refresh()
}
</script>

<template>
  <PageHeader :title="hostname" crumb="Overview">
    <span class="small muted row" style="gap: 8px" aria-live="polite">
      <span v-if="refreshing" class="spinner" aria-hidden="true" style="width: 12px; height: 12px" />
      Refreshes every 10 s
    </span>
  </PageHeader>

  <!-- Needs attention -->
  <section v-if="overview.loading.value" class="card card-pad" aria-label="Loading alerts">
    <SkeletonRows :rows="2" />
  </section>
  <ErrorBox
    v-else-if="overview.error.value && !overview.data.value"
    :message="`Couldn't load alerts: ${overview.error.value}`"
    @retry="overview.refresh()"
  />
  <section
    v-else-if="alerts.length"
    class="card attention"
    :class="{ critical: hasCritical }"
    aria-labelledby="attn"
  >
    <div class="row">
      <AppIcon name="alert" :size="20" :stroke="2" class="attn-icon" />
      <h2 id="attn" style="font-size: 16px">
        {{ alerts.length === 1 ? '1 thing needs' : `${alerts.length} things need` }} your attention
      </h2>
    </div>
    <div class="alert-grid">
      <div v-for="(a, i) in alerts" :key="`${a.title}-${i}`" class="alert-item">
        <StatusPill v-bind="alertTone(a.level)" />
        <span class="alert-text">
          <span class="alert-title">{{ a.title }}</span>
          <span v-if="a.detail" class="small muted">{{ a.detail }}</span>
        </span>
        <NuxtLink v-if="a.link && a.link !== route.path" :to="a.link" class="btn btn-s">Review</NuxtLink>
      </div>
    </div>
  </section>
  <p v-else class="all-good">
    <StatusPill tone="ok" label="All clear" />
    <span class="text-2">Everything looks good. Nothing needs your attention right now.</span>
  </p>

  <!-- Resources -->
  <ErrorBox
    v-if="system.error.value && !sys"
    :message="`Couldn't load server stats: ${system.error.value}`"
    @retry="retryAll"
  />
  <section v-else class="grid-cards" aria-label="Server resources">
    <template v-if="!sys">
      <div v-for="i in 4" :key="i" class="card card-pad stat">
        <SkeletonRows :rows="3" :height="16" label="Loading server stats" />
      </div>
    </template>
    <template v-else>
      <div class="card card-pad stat">
        <p class="lbl">CPU · {{ sys.cpu_count }} {{ sys.cpu_count === 1 ? 'core' : 'cores' }}</p>
        <p class="stat-num">{{ Math.round(sys.cpu_percent) }}<span class="unit">%</span></p>
        <div class="bar" :class="barTone(sys.cpu_percent)" role="img" :aria-label="`CPU ${Math.round(sys.cpu_percent)} percent busy`">
          <span :style="{ width: `${Math.min(100, sys.cpu_percent)}%` }" />
        </div>
        <p class="lbl">{{ sys.os }}</p>
      </div>

      <div class="card card-pad stat">
        <p class="lbl">Memory · {{ formatBytes(sys.memory.total, 0) }}</p>
        <p class="stat-num">{{ Math.round(memPct) }}<span class="unit">%</span></p>
        <div class="bar" :class="barTone(memPct)" role="img" :aria-label="`Memory ${Math.round(memPct)} percent used`">
          <span :style="{ width: `${memPct}%` }" />
        </div>
        <p class="lbl">
          {{ memLine }}
        </p>
      </div>

      <div class="card card-pad stat">
        <p class="lbl">Disk · <span class="mono">{{ rootDisk?.mount ?? '/' }}</span></p>
        <template v-if="rootDisk">
          <p class="stat-num">{{ diskUsed.num }}<span class="unit">{{ diskUsed.rest }}</span></p>
          <div class="bar" :class="barTone(diskPct)" role="img" :aria-label="`Disk ${Math.round(diskPct)} percent used`">
            <span :style="{ width: `${diskPct}%` }" />
          </div>
          <p class="lbl">
            {{ diskLine }}
          </p>
        </template>
        <p v-else class="muted">No disk data reported.</p>
      </div>

      <div class="card card-pad stat">
        <p class="lbl">Uptime</p>
        <p class="stat-num">{{ formatDuration(sys.uptime_secs) }}</p>
        <p class="lbl">Load average (1 · 5 · 15 min)</p>
        <p class="mono text-2" style="font-size: 13px">{{ loadText }}</p>
      </div>
    </template>
  </section>

  <div class="split">
    <!-- Sites -->
    <section class="card" aria-labelledby="sites-h" style="flex: 2 1 600px; overflow: hidden">
      <div class="card-head">
        <h2 id="sites-h">Sites</h2>
        <NuxtLink to="/sites">All sites</NuxtLink>
      </div>
      <div v-if="sites.loading.value" style="padding: 0 20px 20px">
        <SkeletonRows :rows="4" />
      </div>
      <div v-else-if="sites.error.value && !sites.data.value" style="padding: 0 20px 20px">
        <ErrorBox :message="sites.error.value" @retry="sites.refresh()" />
      </div>
      <div v-else-if="!sites.data.value?.length" class="empty">
        <strong>No sites found</strong>
        <span>NexaCloud reads sites from Nginx (<span class="mono">/etc/nginx/sites-enabled</span> and <span class="mono">conf.d</span>).</span>
      </div>
      <div v-else class="table-wrap">
        <table>
          <thead>
            <tr><th>Site</th><th>Type</th><th>PHP</th><th>SSL</th><th>Git</th></tr>
          </thead>
          <tbody>
            <tr v-for="s in sites.data.value" :key="s.id">
              <td>
                <div class="cell-stack">
                  <NuxtLink :to="{ path: '/sites', query: { q: s.id } }" class="site-link">{{ s.server_names[0] ?? s.id }}</NuxtLink>
                  <span v-if="s.root" class="cell-sub mono">{{ s.root }}</span>
                  <span v-else-if="s.proxy_pass" class="cell-sub mono">→ {{ s.proxy_pass }}</span>
                </div>
              </td>
              <td><span class="tag">{{ siteKindLabel(s.kind) }}</span></td>
              <td>
                <span v-if="s.php_version" class="tag mono">PHP {{ s.php_version }}</span>
                <span v-else class="muted">—</span>
              </td>
              <td>
                <span :title="s.ssl_expires ? `Expires ${formatDate(s.ssl_expires)}` : undefined">
                  <StatusPill v-bind="sslStatus(s)" />
                </span>
              </td>
              <td>
                <div v-if="gitRef(s)" class="cell-stack">
                  <span class="mono" style="font-size: 13px">{{ gitRef(s) }}</span>
                  <span v-if="s.git?.message" class="cell-sub truncate">{{ s.git.message }}</span>
                </div>
                <span v-else class="muted">—</span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <!-- Services -->
    <section class="card card-pad" aria-labelledby="svc-h" style="flex: 1 1 300px; display: flex; flex-direction: column; gap: 4px">
      <div class="row" style="justify-content: space-between; margin-bottom: 8px">
        <h2 id="svc-h">Services</h2>
        <NuxtLink to="/services" style="font-size: 13px">Manage</NuxtLink>
      </div>
      <SkeletonRows v-if="services.loading.value" :rows="5" />
      <ErrorBox
        v-else-if="services.error.value && !services.data.value"
        :message="services.error.value"
        @retry="services.refresh()"
      />
      <p v-else-if="!installedServices.length" class="muted" style="padding: 10px 0">No known services are installed.</p>
      <template v-else>
        <div v-for="s in installedServices" :key="s.unit" class="list-row">
          <div class="grow cell-stack">
            <span style="font-weight: 600">{{ s.label }}</span>
            <span class="mono small muted">{{ s.unit }}<template v-if="s.version"> · {{ s.version }}</template></span>
          </div>
          <StatusPill v-bind="serviceStatus(s)" />
        </div>
        <p v-if="missingServices" class="small muted" style="padding-top: 8px">
          {{ missingServices }} known {{ missingServices === 1 ? 'service is' : 'services are' }} not installed.
        </p>
      </template>
    </section>
  </div>
</template>

<style scoped>
.attention {
  padding: 18px 20px; display: flex; flex-direction: column; gap: 12px;
  border-color: #4A3B1C; background: #17140D;
}
.attention .attn-icon { color: var(--warn); }
.attention.critical { border-color: #4A2326; background: #1A1012; }
.attention.critical .attn-icon { color: var(--bad); }
.alert-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(280px, 1fr)); gap: 10px; }
.alert-item {
  display: flex; align-items: center; gap: 12px; padding: 12px 14px;
  background: var(--card); border: 1px solid var(--card-line); border-radius: 10px;
}
.alert-text { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; font-size: 13px; line-height: 1.4; }
.alert-title { color: var(--text); }
.all-good {
  display: flex; flex-wrap: wrap; align-items: center; gap: 12px; padding: 14px 18px;
  border-radius: 12px; border: 1px solid #1D3A2D; background: #0E1713; font-size: 14px;
}
.stat { display: flex; flex-direction: column; gap: 10px; min-height: 168px; }
.stat .bar { margin: 6px 0; }
.site-link { color: var(--text); font-weight: 600; text-decoration: none; }
.site-link:hover { color: var(--link-hover); text-decoration: underline; }
.truncate { max-width: 260px; overflow: hidden; text-overflow: ellipsis; }
</style>
