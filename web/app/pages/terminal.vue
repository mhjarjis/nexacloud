<script setup lang="ts">
import '@xterm/xterm/css/xterm.css'
import type { Terminal } from '@xterm/xterm'
import type { FitAddon } from '@xterm/addon-fit'

useHead({ title: 'Terminal · NexaCloud' })

const api = useApi()
const sudo = useSudo()
const route = useRoute()
const me = useResource<{ role: string; sudo_until: string | null }>('/api/auth/me')
const projects = useResource<{ id: string }[]>('/api/projects')
const host = computed(() => (typeof window !== 'undefined' ? window.location.hostname : 'server'))

const target = ref<string | number | null>(typeof route.query.project === 'string' ? route.query.project : null)
const targetOpts = computed(() => [
  ...(me.data.value?.role === 'admin' ? [{ value: '', label: 'Server (root)', hint: 'full access — be careful' }] : []),
  ...(projects.data.value ?? []).map((p) => ({ value: p.id, label: p.id, hint: "as the project's PHP user, in its folder" })),
])
watch(targetOpts, (o) => { if (target.value === null && o.length) target.value = o[0]!.value }, { immediate: true })

const el = ref<HTMLElement | null>(null)
const state = ref<'idle' | 'connecting' | 'open' | 'closed'>('idle')
const err = ref('')
let term: Terminal | null = null
let fit: FitAddon | null = null
let ws: WebSocket | null = null
let ro: ResizeObserver | null = null

async function ensureTerm() {
  if (term || !el.value) return
  const [{ Terminal }, { FitAddon }] = await Promise.all([import('@xterm/xterm'), import('@xterm/addon-fit')])
  term = new Terminal({
    cursorBlink: true,
    fontFamily: "'JetBrains Mono', ui-monospace, Menlo, monospace",
    fontSize: 13,
    theme: { background: '#07090C', foreground: '#E7ECF2', cursor: '#3DD68C', selectionBackground: '#2A3441' },
    scrollback: 5000,
  })
  fit = new FitAddon()
  term.loadAddon(fit)
  term.open(el.value)
  fit.fit()
  term.onData((d) => { if (ws?.readyState === WebSocket.OPEN) ws.send(new TextEncoder().encode(d)) })
  term.onResize(({ cols, rows }) => { if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ resize: [cols, rows] })) })
  ro = new ResizeObserver(() => { try { fit?.fit() } catch { /* hidden */ } })
  ro.observe(el.value)
}

async function connect() {
  err.value = ''
  disconnect()
  await ensureTerm()
  if (!term) return
  // The socket can't show the password prompt: make sure sudo mode is on first.
  try {
    const m = await api.get<{ sudo_until: string | null }>('/api/auth/me')
    if (!m.sudo_until || new Date(m.sudo_until).getTime() < Date.now() + 5000) await sudo.requestSudo()
  } catch (e) { err.value = errorMessage(e); return }
  state.value = 'connecting'
  term.reset()
  const proto = window.location.protocol === 'https:' ? 'wss' : 'ws'
  const qs = new URLSearchParams({ cols: String(term.cols), rows: String(term.rows) })
  if (target.value) qs.set('project', String(target.value))
  ws = new WebSocket(`${proto}://${window.location.host}/api/terminal?${qs}`)
  ws.binaryType = 'arraybuffer'
  ws.onopen = () => { state.value = 'open'; term?.focus() }
  ws.onmessage = (ev) => {
    if (typeof ev.data === 'string') term?.write(ev.data)
    else term?.write(new Uint8Array(ev.data as ArrayBuffer))
  }
  ws.onclose = () => {
    if (state.value === 'connecting') err.value = 'Could not open the terminal. Your sudo time may have run out, or you are not allowed to open this one.'
    state.value = 'closed'
  }
}
function disconnect() {
  if (ws) { ws.onclose = null; ws.close(); ws = null }
  if (state.value !== 'idle') state.value = 'closed'
}
onBeforeUnmount(() => { disconnect(); ro?.disconnect(); term?.dispose() })
</script>

<template>
  <header style="display: flex; flex-wrap: wrap; align-items: flex-end; justify-content: space-between; gap: 16px">
    <div style="display: flex; flex-direction: column; gap: 6px">
      <p class="lbl">{{ host }} / Terminal</p>
      <h1 style="font-size: 30px; font-weight: 600">Terminal</h1>
    </div>
    <div style="display: flex; flex-wrap: wrap; gap: 8px; align-items: center">
      <div style="width: 260px"><SearchSelect v-model="target" :options="targetOpts" placeholder="Choose where to open" aria-label="Open terminal for" :disabled="state === 'open' || state === 'connecting'" /></div>
      <button v-if="state !== 'open'" type="button" class="btn btn-p" :disabled="state === 'connecting' || target === null" @click="connect"><AppIcon name="terminal" :size="15" /> {{ state === 'connecting' ? 'Connecting…' : state === 'closed' ? 'Reconnect' : 'Open terminal' }}</button>
      <button v-else type="button" class="btn btn-s" @click="disconnect"><AppIcon name="stop" :size="14" /> Disconnect</button>
    </div>
  </header>

  <p v-if="err" class="small" style="color: var(--bad)" role="alert">{{ err }}</p>

  <section class="card" aria-label="Terminal" style="padding: 0; overflow: hidden">
    <div style="display: flex; align-items: center; gap: 10px; padding: 10px 16px; border-bottom: 1px solid #1B222B">
      <span :class="`pill ${state === 'open' ? 'ok' : state === 'connecting' ? 'warn' : ''}`">{{ state === 'open' ? 'Connected' : state === 'connecting' ? 'Connecting' : 'Not connected' }}</span>
      <span class="mono" style="font-size: 13px; color: #A3AEBD">{{ target ? `project ${target}` : target === '' ? 'root@' + host : '' }}</span>
      <span style="flex: 1" />
      <span style="font-size: 12px; color: #A3AEBD">Needs sudo · closes after 30 minutes idle · every session is in Activity</span>
    </div>
    <div ref="el" style="height: 560px; background: #07090C; padding: 8px" />
  </section>
</template>
