<script setup lang="ts">
import type { Service } from '~/types/api'

useHead({ title: 'Services · NexaCloud' })

const api = useApi()
const services = useResource<Service[]>('/api/services', { interval: 15_000 })
const catalog = useResource<import('~/types/api').CatalogItem[]>('/api/catalog')
function onInstalled() { catalog.refresh(); services.refresh() }

const installed = computed(() => (services.data.value ?? []).filter((s) => s.installed))
const missing = computed(() => (services.data.value ?? []).filter((s) => !s.installed))

const busy = ref<Record<string, boolean>>({})
const results = ref<Record<string, { ok: boolean; message: string }>>({})

const TINTS: Record<string, string> = {
  nginx: '#5BE6A9',
  mysql: '#6CB4FF',
  mariadb: '#C5CEDA',
  postgresql: '#8CC4FF',
  'redis-server': '#FF9A9A',
  supervisor: '#B79CFF',
  cron: '#F7C55E',
  fail2ban: '#F7C55E',
  ufw: '#FF9A9A',
}

function tint(unit: string): string {
  if (/^php\d/.test(unit)) return '#B79CFF'
  return TINTS[unit] ?? '#C5CEDA'
}

function abbr(s: Service): string {
  const m = s.unit.match(/^php(\d+\.\d+)/)
  if (m) return 'Php'
  const word = (s.label || s.unit).replace(/[^A-Za-z0-9]/g, '')
  return word.slice(0, 2).replace(/^./, (c) => c.toUpperCase())
}

async function restart(s: Service) {
  busy.value = { ...busy.value, [s.unit]: true }
  const nextResults = { ...results.value }
  delete nextResults[s.unit]
  results.value = nextResults
  try {
    await api.post(`/api/services/${encodeURIComponent(s.unit)}/restart`)
    results.value = { ...results.value, [s.unit]: { ok: true, message: `${s.label} restarted.` } }
    await services.refresh()
  } catch (err) {
    const message = err instanceof SudoCancelled ? 'Not restarted: password not confirmed.' : `Restart failed: ${errorMessage(err)}`
    results.value = { ...results.value, [s.unit]: { ok: false, message } }
  } finally {
    busy.value = { ...busy.value, [s.unit]: false }
  }
}
</script>

<template>
  <PageHeader title="Services" crumb="System services NexaCloud knows about">
    <span v-if="services.data.value" class="small muted">
      {{ installed.filter((s) => s.active).length }} of {{ installed.length }} running
    </span>
  </PageHeader>

  <div v-if="services.loading.value" class="grid-fill">
    <div v-for="i in 6" :key="i" class="card card-pad"><SkeletonRows :rows="3" label="Loading services" /></div>
  </div>
  <ErrorBox
    v-else-if="services.error.value && !services.data.value"
    :message="services.error.value"
    :retrying="services.refreshing.value"
    @retry="services.refresh()"
  />
  <template v-else>
    <section aria-labelledby="inst-h" class="stack">
      <h2 id="inst-h" class="section-h">Installed</h2>
      <p v-if="!installed.length" class="card empty">None of the known services are installed.</p>
      <div v-else class="grid-fill">
        <article v-for="s in installed" :key="s.unit" class="card svc" :aria-label="s.label">
          <div class="row" style="gap: 10px">
            <div class="svc-badge" :style="{ color: tint(s.unit) }" aria-hidden="true">{{ abbr(s) }}</div>
            <div class="grow cell-stack">
              <span class="svc-name">{{ s.label }}</span>
              <span class="mono small muted svc-meta" :title="s.version ? `${s.unit} · ${s.version}` : s.unit">{{ s.unit }}<template v-if="s.version"> · {{ s.version }}</template></span>
            </div>
            <StatusPill v-bind="serviceStatus(s)" />
          </div>
          <div class="row wrap" style="gap: 10px; min-height: 36px">
            <ConfirmAction
              label="Restart"
              :question="`Restart ${s.label}?`"
              :busy="busy[s.unit]"
              :aria-label="`Restart ${s.label}`"
              @confirm="restart(s)"
            >
              <template #icon><AppIcon v-if="!busy[s.unit]" name="restart" :size="14" :stroke="2" /></template>
            </ConfirmAction>
            <span v-if="busy[s.unit]" class="small muted" aria-live="polite">Restarting…</span>
            <span
              v-else-if="results[s.unit]"
              class="small"
              :style="{ color: results[s.unit]!.ok ? 'var(--ok)' : 'var(--bad)' }"
              role="status"
            >{{ results[s.unit]!.message }}</span>
          </div>
        </article>
      </div>
    </section>

    <section v-if="missing.length" aria-labelledby="miss-h" class="stack">
      <h2 id="miss-h" class="section-h">Not installed</h2>
      <div class="grid-fill">
        <div v-for="s in missing" :key="s.unit" class="card svc svc-off">
          <div class="row" style="gap: 10px">
            <div class="svc-badge" aria-hidden="true">{{ abbr(s) }}</div>
            <div class="grow cell-stack">
              <span class="svc-name">{{ s.label }}</span>
              <span class="mono small muted svc-meta" :title="s.unit">{{ s.unit }}</span>
            </div>
            <StatusPill v-bind="serviceStatus(s)" />
          </div>
        </div>
      </div>
      <section aria-labelledby="cat-h" class="stack" style="gap: 12px; margin-top: 8px">
        <h2 id="cat-h" style="font-size: 17px">Install software</h2>
        <div v-if="catalog.data.value" class="cat-grid">
          <article v-for="c in catalog.data.value" :key="c.id" class="card cat">
            <div class="row" style="justify-content: space-between; gap: 8px">
              <strong>{{ c.label }}</strong>
              <StatusPill :tone="c.installed ? 'ok' : 'off'" :label="c.installed ? 'Installed' : 'Not installed'" />
            </div>
            <span class="small text-2">{{ c.description }}</span>
            <span class="small muted mono">apt: {{ c.packages.join(' ') }}</span>
            <PlanAction
              v-if="!c.installed"
              :request="{ kind: 'service_install', params: { id: c.id } }"
              label="Install"
              :success-text="`${c.label} is installed.`"
              @done="onInstalled"
            />
          </article>
        </div>
      </section>
    </section>
  </template>
</template>

<style scoped>
.cat-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 12px; }
.cat { padding: 16px; display: flex; flex-direction: column; gap: 8px; }
.section-h { font-size: 18px; }
.svc { padding: 16px; display: flex; flex-direction: column; gap: 12px; }
.svc-off { gap: 0; }
.svc-off .svc-name { color: var(--text-2); }
.svc-badge {
  width: 36px; height: 36px; border-radius: 9px; background: var(--tag-bg); display: grid; place-items: center;
  font-family: var(--font-head); font-weight: 700; font-size: 13px; color: var(--muted); flex: none;
}
.svc-name { font-size: 15px; font-weight: 600; }
/* unit · version stays on one line; long values end in an ellipsis */
.svc-meta { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>