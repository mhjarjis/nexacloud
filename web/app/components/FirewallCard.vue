<script setup lang="ts">
// Turn UFW on with safe defaults + the 3-minute anti-lockout confirmation.
const props = defineProps<{ active: boolean }>()
const emit = defineEmits<{ changed: [] }>()
const api = useApi()
const armed = ref(false)
const left = ref(180)
const err = ref<string | null>(null)
let t: ReturnType<typeof setInterval> | undefined

async function poll() {
  try { armed.value = (await api.get<{ armed: boolean }>('/api/firewall/guard')).armed } catch { /* ignore */ }
}
function startCountdown() {
  left.value = 180
  clearInterval(t)
  t = setInterval(() => { left.value = Math.max(0, left.value - 1); if (left.value % 10 === 0) poll() }, 1000)
}
async function onDone(s: string) {
  if (s === 'applied') { await poll(); if (armed.value) startCountdown(); emit('changed') }
}
async function keep() {
  try { await api.post('/api/firewall/confirm'); armed.value = false; clearInterval(t); emit('changed') } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) }
}
onMounted(poll)
onBeforeUnmount(() => clearInterval(t))
</script>

<template>
  <section class="card card-pad stack" style="gap: 12px" aria-labelledby="fw-h">
    <div class="row wrap" style="gap: 10px; justify-content: space-between">
      <h2 id="fw-h">Firewall</h2>
      <StatusPill :tone="active ? 'ok' : 'bad'" :label="active ? 'On' : 'Off'" />
    </div>
    <template v-if="armed">
      <div class="guard stack" style="gap: 8px">
        <strong style="color: var(--warn)">Can you still use the panel? Click “Keep it on” within {{ Math.floor(left / 60) }}:{{ String(left % 60).padStart(2, '0') }}.</strong>
        <span class="small text-2">If you don't, the firewall turns itself off again automatically — so a wrong rule can never lock you out.</span>
        <div><button type="button" class="btn btn-p" @click="keep">Keep it on</button></div>
      </div>
    </template>
    <template v-else-if="!active">
      <p class="small text-2">Blocks every port except SSH (rate-limited against password guessing), HTTP, HTTPS and this panel. Databases and Redis become unreachable from the internet.</p>
      <PlanAction :request="{ kind: 'firewall_enable', params: { extra_ports: [] } }" label="Preview" btn-class="btn btn-s btn-p" apply-label="Turn on firewall" success-text="Firewall is on." @done="onDone" />
    </template>
    <p v-else class="small text-2">Only SSH, HTTP, HTTPS and the panel are reachable. Rules are listed below.</p>
    <p v-if="err" class="small" style="color: var(--bad)">{{ err }}</p>
  </section>
</template>

<style scoped>
.guard { padding: 14px; border: 1px solid var(--warn); border-radius: 10px; background: rgba(245,184,61,.06); }
</style>
