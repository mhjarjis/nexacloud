<script setup lang="ts">
import type { AdvisorReport, Finding } from '~/types/api'

useHead({ title: 'Advisor · NexaCloud' })

const api = useApi()
const report = useResource<AdvisorReport>('/api/advisor')
const showDismissed = ref(false)
const busy = ref<string | null>(null)

const scoreTone = computed(() => {
  const s = report.data.value?.score ?? 0
  return s >= 85 ? 'ok' : s >= 60 ? 'warn' : 'bad'
})
const groups = computed(() => {
  const f = report.data.value?.findings ?? []
  return (['critical', 'warning', 'info'] as const)
    .map((sev) => ({ sev, items: f.filter((x) => x.severity === sev) }))
    .filter((g) => g.items.length)
})
const TIER: Record<Finding['tier'], string> = {
  safe: 'One click, no downtime',
  restart: 'Restarts a service (a few seconds)',
  manual: 'Needs your decision',
}
const SEV = { critical: 'Critical', warning: 'Recommended', info: 'Nice to have' } as const

async function dismiss(f: Finding, days: number | null) {
  busy.value = f.id
  try {
    await api.post(`/api/advisor/${encodeURIComponent(f.id)}/dismiss`, { days })
    await report.refresh()
  } finally {
    busy.value = null
  }
}
</script>

<template>
  <PageHeader title="Advisor" crumb="Recommendations for this server">
    <button type="button" class="btn btn-s" :disabled="report.refreshing.value" @click="report.refresh()">
      <span v-if="report.refreshing.value" class="spinner" aria-hidden="true" />
      <AppIcon v-else name="restart" :size="14" :stroke="2" /> Re-check
    </button>
  </PageHeader>

  <div v-if="report.loading.value" class="card card-pad"><SkeletonRows :rows="6" label="Checking the server" /></div>
  <ErrorBox v-else-if="report.error.value && !report.data.value" :message="report.error.value" :retrying="report.refreshing.value" @retry="report.refresh()" />

  <template v-else-if="report.data.value">
    <section class="card card-pad score-card" aria-label="Server score">
      <div class="score" :class="scoreTone">{{ report.data.value.score }}</div>
      <div class="stack" style="gap: 4px">
        <strong style="font-size: 17px">
          {{ report.data.value.findings.length ? `${report.data.value.findings.length} recommendation${report.data.value.findings.length === 1 ? '' : 's'}` : 'Everything checks out' }}
        </strong>
        <span class="small muted">Tuned for {{ Math.round(report.data.value.ram_bytes / 2 ** 30) }} GB RAM. Every fix shows exactly what it changes first, and can be rolled back from Activity.</span>
      </div>
    </section>

    <section v-for="g in groups" :key="g.sev" class="stack" style="gap: 10px" :aria-label="SEV[g.sev]">
      <h2 class="grp">{{ SEV[g.sev] }}</h2>
      <article v-for="f in g.items" :key="f.id" class="card finding">
        <div class="row wrap" style="gap: 10px; justify-content: space-between">
          <div class="row wrap" style="gap: 10px">
            <StatusPill v-bind="alertTone(f.severity)" />
            <strong>{{ f.title }}</strong>
          </div>
          <span class="tag">{{ f.category }}</span>
        </div>
        <p class="text-2" style="font-size: 14px; line-height: 1.5">{{ f.detail }}</p>
        <div v-if="f.current || f.recommended" class="cmp">
          <div v-if="f.current"><span class="lbl">Now</span><span class="mono">{{ f.current }}</span></div>
          <div v-if="f.recommended"><span class="lbl">Recommended</span><span class="mono" style="color: var(--ok)">{{ f.recommended }}</span></div>
          <div><span class="lbl">Fix</span><span class="small">{{ TIER[f.tier] }}</span></div>
        </div>
        <div class="row wrap" style="gap: 10px; align-items: flex-start">
          <PlanAction
            v-if="f.fixable"
            :request="{ kind: 'advisor_fix', params: { rule: f.id } }"
            label="Preview fix"
            btn-class="btn btn-s btn-p"
            apply-label="Apply fix"
            success-text="Fixed."
            style="flex: 1 1 100%"
            @done="report.refresh()"
          />
          <NuxtLink v-if="f.link" :to="f.link" class="btn btn-s">Open</NuxtLink>
          <button type="button" class="btn btn-s btn-ghost" :disabled="busy === f.id" @click="dismiss(f, 7)">Snooze 7 days</button>
          <button type="button" class="btn btn-s btn-ghost" :disabled="busy === f.id" @click="dismiss(f, null)">Dismiss</button>
        </div>
      </article>
    </section>

    <section v-if="report.data.value.dismissed.length" class="stack" style="gap: 8px">
      <button type="button" class="btn btn-s btn-ghost" style="align-self: flex-start" @click="showDismissed = !showDismissed">
        {{ showDismissed ? 'Hide' : 'Show' }} {{ report.data.value.dismissed.length }} dismissed
      </button>
      <template v-if="showDismissed">
        <div v-for="f in report.data.value.dismissed" :key="f.id" class="card card-pad row wrap" style="gap: 10px; justify-content: space-between">
          <span>{{ f.title }} <span class="small muted">· {{ f.dismissed_until === 'forever' ? 'dismissed' : `snoozed until ${formatDate(f.dismissed_until!)}` }}</span></span>
          <button type="button" class="btn btn-s" :disabled="busy === f.id" @click="dismiss(f, -1)">Bring back</button>
        </div>
      </template>
    </section>
  </template>
</template>

<style scoped>
.score-card { display: flex; align-items: center; gap: 20px; flex-wrap: wrap; }
.score { width: 76px; height: 76px; border-radius: 50%; display: grid; place-items: center; font-family: var(--font-head); font-size: 30px; font-weight: 600; border: 4px solid; }
.score.ok { color: var(--ok); border-color: var(--ok); }
.score.warn { color: var(--warn); border-color: var(--warn); }
.score.bad { color: var(--bad); border-color: var(--bad); }
.grp { font-size: 15px; color: var(--text-2); margin-top: 6px; }
.finding { padding: 16px 18px; display: flex; flex-direction: column; gap: 10px; }
.cmp { display: flex; flex-wrap: wrap; gap: 10px 28px; padding: 10px 12px; background: var(--sunken, #0F1318); border-radius: 10px; }
.cmp > div { display: flex; flex-direction: column; gap: 3px; }
</style>
