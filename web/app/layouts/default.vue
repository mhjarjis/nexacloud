<script setup lang="ts">
import type { Health, SystemInfo } from '~/types/api'
import type { IconName } from '~/utils/icons'

const NAV: { to: string; label: string; icon: IconName }[] = [
  { to: '/', label: 'Dashboard', icon: 'dashboard' },
  { to: '/sites', label: 'Sites', icon: 'sites' },
  { to: '/projects', label: 'Projects', icon: 'git' },
  { to: '/php', label: 'PHP', icon: 'php' },
  { to: '/services', label: 'Services', icon: 'services' },
  { to: '/databases', label: 'Databases', icon: 'databases' },
  { to: '/backups', label: 'Backups', icon: 'backup' },
  { to: '/monitoring', label: 'Monitoring', icon: 'pulse' },
  { to: '/advisor', label: 'Advisor', icon: 'advisor' },
  { to: '/security', label: 'Security', icon: 'security' },
  { to: '/activity', label: 'Activity', icon: 'activity' },
]

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
              <span class="chip-meta">{{ system.data.value.os }}</span>
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
        </div>
      </div>

      <nav class="nav" aria-label="Main">
        <NuxtLink v-for="item in NAV" :key="item.to" :to="item.to" class="nav-item">
          <AppIcon :name="item.icon" />
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
