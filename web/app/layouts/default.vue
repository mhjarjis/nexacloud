<script setup lang="ts">
import type { Health, SystemInfo } from '~/types/api'
import type { IconName } from '~/utils/icons'

const NAV: { to: string; label: string; icon: IconName }[] = [
  { to: '/', label: 'Dashboard', icon: 'dashboard' },
  { to: '/projects', label: 'Projects', icon: 'projects' },
  { to: '/domains', label: 'Domains & SSL', icon: 'globe' },
  { to: '/deployments', label: 'Deployments', icon: 'git' },
  { to: '/php', label: 'PHP & Services', icon: 'runtimes' },
  { to: '/databases', label: 'Databases', icon: 'databases' },
  { to: '/backups', label: 'Backups & Storage', icon: 'backup' },
  { to: '/install', label: 'Install & Import', icon: 'terminal' },
]
const MORE: { to: string; label: string; icon: IconName }[] = [
  { to: '/files', label: 'File manager', icon: 'folder' },
  { to: '/terminal', label: 'Terminal', icon: 'terminal' },
  { to: '/cron', label: 'Cron jobs', icon: 'clock' },
  { to: '/monitoring', label: 'Monitoring', icon: 'pulse' },
  { to: '/advisor', label: 'Advisor', icon: 'advisor' },
  { to: '/security', label: 'Security', icon: 'security' },
  { to: '/activity', label: 'Activity', icon: 'activity' },
  { to: '/account', label: 'Account & users', icon: 'users' },
]
const route = useRoute()
function isActive(to: string) {
  if (to === '/') return route.path === '/'
  if (to === '/php') return route.path.startsWith('/php') || route.path.startsWith('/services')
  if (to === '/domains') return route.path.startsWith('/domains') || route.path.startsWith('/sites')
  if (to === '/projects') return route.path.startsWith('/projects') || route.path.startsWith('/apps')
  return route.path.startsWith(to)
}
const monitor = useResource<{ last_code: number | null }[]>('/api/monitor', { interval: 60_000 })
const downCount = computed(() => (monitor.data.value ?? []).filter((m) => m.last_code !== null && (m.last_code === 0 || m.last_code >= 500)).length)

const api = useApi()
const authed = useAuthState()
const health = useResource<Health>('/api/health', { interval: 5_000 })
const system = useResource<SystemInfo>('/api/system')

const agentUp = computed(() => !health.error.value && !!health.data.value?.ok)
const agentLabel = computed(() =>
  health.data.value ? `nexacloud-agent ${health.data.value.version} · Rust` : 'nexacloud-agent · Rust',
)
const lastBeat = ref<number | null>(null)
const now = ref(Date.now())
watch(() => health.data.value, (v) => { if (v?.ok) lastBeat.value = Date.now() }, { immediate: true })
let beatTimer: ReturnType<typeof setInterval> | null = null
onMounted(() => { beatTimer = setInterval(() => { now.value = Date.now() }, 1000) })
onBeforeUnmount(() => { if (beatTimer) clearInterval(beatTimer) })
const beatLabel = computed(() => lastBeat.value === null ? '' : `Heartbeat ${Math.max(0, Math.round((now.value - lastBeat.value) / 1000))}s ago`)
const signingOut = ref(false)

async function signOut() {
  signingOut.value = true
  try {
    await api.post('/api/auth/logout')
  } catch {
    // The cookie is cleared server-side when reachable; go to /login either way.
  } finally {
    authed.value = false
    signingOut.value = false
    await navigateTo('/login')
  }
}
</script>

<template>
  <div class="shell">
    <aside class="sidebar" aria-label="Sidebar">
      <div class="sidebar-top">
        <NuxtLink to="/" class="brand" aria-label="NexaCloud dashboard">
          <LogoMark />
          <span class="brand-name">NexaCloud</span>
        </NuxtLink>
        <div class="server-chip">
          <span class="dot" :class="{ down: !agentUp && !health.loading.value }" aria-hidden="true" />
          <span class="chip-text">
            <template v-if="system.data.value">
              <span class="chip-name">{{ system.data.value.hostname }}</span>
              <span class="chip-meta">{{ system.data.value.os.replace(/\.\d+ LTS$/, '') }} · {{ system.data.value.cpu_count }} vCPU · {{ Math.round(system.data.value.memory.total / 1024 ** 3) }} GB</span>
            </template>
            <template v-else-if="system.loading.value">
              <span class="skel" style="height: 14px; width: 70%" />
              <span class="skel" style="height: 11px; width: 50%" />
            </template>
            <template v-else>
              <span class="chip-name">This server</span>
              <span class="chip-meta">Details unavailable</span>
            </template>
          </span>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="#A3AEBD" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M8 9l4-4 4 4M8 15l4 4 4-4" /></svg>
        </div>
      </div>

      <nav class="nav" aria-label="Main">
        <NuxtLink v-for="item in NAV" :key="item.to" :to="item.to" class="nav-item" :aria-current="isActive(item.to) ? 'page' : undefined">
          <AppIcon :name="item.icon" />
          <span class="nav-label">{{ item.label }}</span>
          <span v-if="item.to === '/projects' && downCount" class="nav-badge" :title="`${downCount} down`">{{ downCount }}</span>
        </NuxtLink>
        <span class="nav-sep" aria-hidden="true">More</span>
        <NuxtLink v-for="item in MORE" :key="item.to" :to="item.to" class="nav-item nav-more" :aria-current="isActive(item.to) ? 'page' : undefined">
          <AppIcon :name="item.icon" :size="16" />
          <span class="nav-label">{{ item.label }}</span>
        </NuxtLink>
      </nav>

      <div class="agent-box">
        <div class="stack" style="gap: 6px">
          <StatusPill
            v-if="health.loading.value"
            tone="off"
            label="Checking agent"
            style="align-self: flex-start"
          />
          <StatusPill
            v-else
            :tone="agentUp ? 'ok' : 'bad'"
            :label="agentUp ? 'Agent connected' : 'Agent unreachable'"
            style="align-self: flex-start"
          />
          <span class="small muted">{{ agentLabel }}</span>
          <span v-if="beatLabel" class="small muted">{{ beatLabel }}</span>
        </div>
        <button type="button" class="btn btn-s" :disabled="signingOut" @click="signOut">
          <AppIcon name="logout" :size="16" />
          Sign out
        </button>
      </div>
    </aside>

    <main id="main" class="main">
      <SudoPrompt />
      <slot />
    </main>
  </div>
</template>
