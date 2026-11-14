<script setup lang="ts">
// Visitor stats from nginx logs (no tracking script).
interface Day { date: string; requests: number; visitors: number; bytes: number }
interface Top { name: string; count: number }
interface Stats { enabled: boolean; days?: Day[]; totals?: { requests: number; human_requests: number; bot_requests: number; visitors: number; bytes: number }; top_pages?: Top[]; top_referrers?: Top[]; status?: Top[]; browsers?: Top[] }

const props = defineProps<{ siteId: string }>()
const api = useApi()
const days = ref(7)
const data = ref<Stats | null>(null)
const loading = ref(false)
const err = ref('')
async function load() {
  loading.value = true; err.value = ''
  try { data.value = await api.get<Stats>(`/api/sites/${encodeURIComponent(props.siteId)}/stats?days=${days.value}`) } catch (e) { err.value = errorMessage(e) } finally { loading.value = false }
}
watch([() => props.siteId, days], load, { immediate: true })

const enabling = ref(false)
async function enable() {
  enabling.value = true; err.value = ''
  try {
    const req = { kind: 'stats_enable', params: {} }
    const pv = await api.post<{ plan_hash: string }>('/api/plans/preview', req)
    const st = await api.post<{ change_id: string }>('/api/plans/apply', { ...req, plan_hash: pv.plan_hash })
    for (let i = 0; i < 60; i++) {
      const c = await api.get<{ status: string; error?: string | null }>(`/api/changes/${st.change_id}`)
      if (['applied', 'reverted', 'failed', 'revert_failed'].includes(c.status)) { if (c.status !== 'applied') err.value = `${c.status}: ${c.error ?? 'see Activity'}`; break }
      await new Promise((r) => setTimeout(r, 1000))
    }
    await load()
  } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) } finally { enabling.value = false }
}

const maxV = computed(() => Math.max(1, ...(data.value?.days ?? []).map((d) => d.requests)))
const cards = computed(() => {
  const t = data.value?.totals
  return [
    { k: 'Visitors', v: t ? t.visitors.toLocaleString() : '—' },
    { k: 'Page requests', v: t ? t.human_requests.toLocaleString() : '—' },
    { k: 'Bots & crawlers', v: t ? t.bot_requests.toLocaleString() : '—' },
    { k: 'Data sent', v: t ? formatBytes(t.bytes) : '—' },
  ]
})
function short(d: string) { return d.slice(5).replace('-', '/') }
</script>

<template>
  <div class="stack" style="gap: 16px">
    <p v-if="err" class="small" style="color: var(--bad)" role="alert">{{ err }}</p>
    <section v-if="data && !data.enabled" class="card card-pad stack" style="gap: 12px">
      <div class="row" style="gap: 10px"><AppIcon name="activity" :size="18" /><h2 style="font-size: 17px; font-weight: 600">Visitor stats are off</h2></div>
      <p class="small muted">Turning them on adds one shared nginx access log (kept 30 days). Stats are counted on this server from that log — no tracking script, no cookies, nothing sent anywhere. Counting starts from now.</p>
      <button type="button" class="btn btn-p" style="align-self: flex-start" :disabled="enabling" @click="enable">{{ enabling ? 'Turning on…' : 'Turn on visitor stats' }}</button>
    </section>
    <template v-else-if="data?.days">
      <div class="row" style="justify-content: space-between; flex-wrap: wrap; gap: 10px">
        <div role="group" aria-label="Period" style="display: flex; gap: 4px; padding: 4px; background: #12171D; border: 1px solid #222A34; border-radius: 10px">
          <button v-for="d in [1, 7, 30]" :key="d" type="button" :aria-pressed="days === d" :style="`min-height: 32px; padding: 0 14px; border-radius: 7px; border: 0; cursor: pointer; font-size: 13px; font-weight: 600; background: ${days === d ? '#1F2833' : 'transparent'}; color: ${days === d ? '#E7ECF2' : '#A3AEBD'}`" @click="days = d">{{ d === 1 ? 'Today' : `${d} days` }}</button>
        </div>
        <button type="button" class="btn btn-s" :disabled="loading" @click="load"><AppIcon name="refresh" :size="14" /> Refresh</button>
      </div>
      <section style="display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 12px">
        <div v-for="c in cards" :key="c.k" class="card" style="padding: 16px; display: flex; flex-direction: column; gap: 6px">
          <span class="lbl">{{ c.k }}</span>
          <span style="font-family: 'Space Grotesk', sans-serif; font-size: 24px; font-weight: 600">{{ c.v }}</span>
        </div>
      </section>
      <section v-if="days > 1" class="card card-pad stack" aria-label="Requests per day" style="gap: 10px">
        <span class="lbl">Page requests per day</span>
        <div style="display: flex; align-items: flex-end; gap: 4px; height: 140px">
          <div v-for="d in data.days" :key="d.date" style="flex: 1; display: flex; flex-direction: column; align-items: center; gap: 4px; min-width: 0" :title="`${d.date}: ${d.requests} requests, ${d.visitors} visitors`">
            <div :style="`width: 100%; max-width: 32px; height: ${Math.max(2, Math.round((d.requests / maxV) * 110))}px; background: #3DDC97; border-radius: 4px 4px 0 0; opacity: ${d.requests ? 1 : .25}`" />
            <span v-if="days <= 14" class="mono" style="font-size: 10px; color: #A3AEBD">{{ short(d.date) }}</span>
          </div>
        </div>
      </section>
      <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr)); gap: 16px">
        <section v-for="[title, list] in ([['Top pages', data.top_pages], ['Top referrers', data.top_referrers], ['Responses', data.status], ['Browsers', data.browsers]] as [string, Top[] | undefined][])" :key="title" class="card card-pad stack" style="gap: 8px">
          <span class="lbl">{{ title }}</span>
          <div v-for="t in list ?? []" :key="t.name" class="row" style="justify-content: space-between; gap: 10px; font-size: 13px">
            <span class="mono" style="overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0">{{ t.name }}</span><span class="mono muted">{{ t.count.toLocaleString() }}</span>
          </div>
          <p v-if="!(list ?? []).length" class="small muted">Nothing yet.</p>
        </section>
      </div>
    </template>
    <p v-else class="small muted">Loading…</p>
  </div>
</template>
