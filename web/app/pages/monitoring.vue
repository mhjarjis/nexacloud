<script setup lang="ts">
useHead({ title: 'Monitoring · NexaCloud' })
const api = useApi()

interface Bucket { ts: number; p50: number | null; p95: number | null; checks: number; failures: number }
interface Stats { site: string; checks: number; uptime: number | null; last_code: number | null; last_ms: number | null; last_at: number | null; failures_1h: number; checks_1h: number; buckets: Bucket[]; interval_secs: number }
interface Notification { id: string; at: string; level: string; key: string; title: string; body: string; sent: string }
interface Settings {
  telegram: { enabled: boolean; bot_token: string; chat_id: string; has_token?: boolean }
  email: { enabled: boolean; smtp_host: string; smtp_port: number; security: string; user: string; password: string; from: string; to: string; has_password?: boolean }
  events: { site_down: boolean; deploy_failed: boolean; backup_failed: boolean; server: boolean }
}

const sites = useResource<Stats[]>('/api/monitor', { interval: 30_000 })
const notes = useResource<Notification[]>('/api/notifications', { interval: 30_000 })
const loaded = useResource<Settings>('/api/notify/settings')
const form = ref<Settings | null>(null)
watch(() => loaded.data.value, (v) => { if (v && !form.value) form.value = JSON.parse(JSON.stringify(v)) }, { immediate: true })

function siteTone(s: Stats) {
  if (s.last_code === null) return { tone: 'off' as const, label: 'Waiting' }
  if (s.last_code === 0) return { tone: 'bad' as const, label: 'Down' }
  if (s.last_code >= 500) return { tone: 'bad' as const, label: `HTTP ${s.last_code}` }
  return { tone: 'ok' as const, label: `Up · ${s.last_code}` }
}
function spark(s: Stats) {
  const vals = s.buckets.map((b) => b.p50)
  const max = Math.max(50, ...vals.filter((v): v is number => v !== null))
  let d = ''
  let pen = false
  vals.forEach((v, i) => {
    if (v === null) { pen = false; return }
    d += `${pen ? 'L' : 'M'}${(i / Math.max(1, vals.length - 1)) * 120},${(28 - (v / max) * 24).toFixed(1)} `
    pen = true
  })
  return d.trim()
}
const downCount = computed(() => (sites.data.value ?? []).filter((s) => s.last_code !== null && (s.last_code === 0 || s.last_code >= 500)).length)

const saving = ref(false)
const msg = ref<{ ok: boolean; text: string } | null>(null)
async function save() {
  if (!form.value) return
  saving.value = true; msg.value = null
  try {
    form.value = JSON.parse(JSON.stringify(await api.put<Settings>('/api/notify/settings', form.value)))
    msg.value = { ok: true, text: 'Saved.' }
  } catch (e) { if (!(e instanceof SudoCancelled)) msg.value = { ok: false, text: errorMessage(e) } } finally { saving.value = false }
}
const testing = ref(false)
const testResult = ref<{ channel: string; ok: boolean; error: string | null }[] | null>(null)
async function test() {
  testing.value = true; testResult.value = null; msg.value = null
  try { testResult.value = await api.post('/api/notify/test', {}) } catch (e) { if (!(e instanceof SudoCancelled)) msg.value = { ok: false, text: errorMessage(e) } } finally { testing.value = false }
}
function noteTone(l: string) { return l === 'critical' ? 'bad' : l === 'warning' ? 'warn' : 'info' }
</script>

<template>
  <PageHeader title="Monitoring" crumb="Uptime & alerts" />

  <section class="card" aria-labelledby="up" style="overflow: hidden">
    <div class="row wrap top">
      <h2 id="up">Sites</h2>
      <span class="small muted">Checked every {{ sites.data.value?.[0]?.interval_secs ?? 60 }} s from this server. Three failures in a row send a “site down” alert.</span>
      <StatusPill v-if="downCount" tone="bad" :label="`${downCount} down`" />
    </div>
    <SkeletonRows v-if="sites.loading.value && !sites.data.value" :rows="3" />
    <div v-else-if="!(sites.data.value ?? []).length" class="empty"><span>No sites to check yet.</span></div>
    <div v-else class="table-wrap">
      <table>
        <thead><tr><th>Site</th><th>Status</th><th>Response</th><th>Uptime 24 h</th><th>Failed · 1 h</th><th>Last 24 h (median)</th></tr></thead>
        <tbody>
          <tr v-for="s in sites.data.value" :key="s.site">
            <td class="mono" style="font-size: 13px">{{ s.site }}</td>
            <td><StatusPill v-bind="siteTone(s)" /></td>
            <td class="mono">{{ s.last_ms !== null ? `${s.last_ms} ms` : '—' }}</td>
            <td class="mono">{{ s.uptime !== null ? `${(s.uptime * 100).toFixed(2)}%` : '—' }}</td>
            <td class="mono">{{ s.failures_1h }} / {{ s.checks_1h }}</td>
            <td><svg width="120" height="30" viewBox="0 0 120 30" role="img" :aria-label="`Response times for ${s.site}`"><path :d="spark(s)" fill="none" stroke="#6CB4FF" stroke-width="1.6" /></svg></td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <div class="split">
    <section v-if="form" class="card card-pad stack grow-card" aria-labelledby="ch" style="gap: 16px">
      <div class="stack" style="gap: 4px">
        <h2 id="ch">Where to send alerts</h2>
        <p class="small muted">Tokens and passwords are stored on this server only and are never shown again. Leave a secret empty to keep the saved one.</p>
      </div>

      <fieldset class="chan">
        <legend><label class="row" style="gap: 10px"><input v-model="form.telegram.enabled" type="checkbox"> <strong>Telegram</strong></label></legend>
        <p class="small muted">Create a bot with <span class="mono">@BotFather</span>, send it a message, then put your chat id here (or a channel like <span class="mono">@mychannel</span> with the bot as admin).</p>
        <div class="fgrid">
          <label class="field"><span>Bot token</span><input v-model="form.telegram.bot_token" class="in mono" type="password" autocomplete="off" :placeholder="form.telegram.has_token ? 'saved — leave empty to keep' : '123456:ABC…'"></label>
          <label class="field"><span>Chat id</span><input v-model="form.telegram.chat_id" class="in mono" type="text" placeholder="-1001234567890"></label>
        </div>
      </fieldset>

      <fieldset class="chan">
        <legend><label class="row" style="gap: 10px"><input v-model="form.email.enabled" type="checkbox"> <strong>Email (SMTP)</strong></label></legend>
        <div class="fgrid">
          <label class="field"><span>SMTP server</span><input v-model="form.email.smtp_host" class="in mono" type="text" placeholder="smtp.mailgun.org"></label>
          <label class="field"><span>Port</span><input v-model.number="form.email.smtp_port" class="in mono" type="number" min="1" max="65535" placeholder="587"></label>
          <label class="field"><span>Security</span>
            <SearchSelect v-model="form.email.security" :options="[{ value: 'starttls', label: 'STARTTLS (587)' }, { value: 'tls', label: 'TLS (465)' }, { value: 'none', label: 'None (local relay only)' }]" aria-label="Security" />
          </label>
          <label class="field"><span>Username</span><input v-model="form.email.user" class="in mono" type="text" autocomplete="off" placeholder="postmaster@mg.example.com"></label>
          <label class="field"><span>Password</span><input v-model="form.email.password" class="in mono" type="password" autocomplete="new-password" :placeholder="form.email.has_password ? 'saved — leave empty to keep' : ''"></label>
          <label class="field"><span>From</span><input v-model="form.email.from" class="in mono" type="email" placeholder="alerts@example.com"></label>
          <label class="field"><span>To</span><input v-model="form.email.to" class="in mono" type="email" placeholder="you@example.com"></label>
        </div>
      </fieldset>

      <fieldset class="chan">
        <legend><strong>Send alerts for</strong></legend>
        <div class="toggles">
          <label class="tog"><input v-model="form.events.site_down" type="checkbox"> A site goes down or comes back</label>
          <label class="tog"><input v-model="form.events.deploy_failed" type="checkbox"> A deploy fails or is rolled back</label>
          <label class="tog"><input v-model="form.events.backup_failed" type="checkbox"> A backup fails</label>
          <label class="tog"><input v-model="form.events.server" type="checkbox"> Server warnings (disk, SSL expiry, services down) — at most daily</label>
        </div>
      </fieldset>

      <div class="row wrap" style="gap: 10px">
        <button type="button" class="btn btn-p" :disabled="saving" @click="save">Save</button>
        <button type="button" class="btn" :disabled="testing" @click="test">{{ testing ? 'Sending…' : 'Send a test' }}</button>
        <span v-if="msg" class="small" :style="{ color: msg.ok ? 'var(--ok)' : 'var(--bad)' }" role="status">{{ msg.text }}</span>
      </div>
      <ul v-if="testResult" class="plain">
        <li v-for="r in testResult" :key="r.channel" class="small"><StatusPill :tone="r.ok ? 'ok' : 'bad'" :label="r.channel" /> {{ r.ok ? 'delivered' : r.error }}</li>
      </ul>
    </section>

    <section class="card card-pad stack grow-card" aria-labelledby="rec" style="gap: 10px">
      <h2 id="rec">Recent alerts</h2>
      <div v-if="!(notes.data.value ?? []).length" class="empty"><span>Nothing yet. Alerts show up here even when no channel is set up.</span></div>
      <div v-for="n in notes.data.value ?? []" :key="n.id" class="note-row">
        <div class="row wrap" style="gap: 8px"><StatusPill :tone="noteTone(n.level)" :label="n.level" /><strong style="font-size: 14px">{{ n.title }}</strong></div>
        <span class="small text-2">{{ n.body }}</span>
        <span class="small muted">{{ formatRelative(n.at) }}<template v-if="n.sent"> · {{ n.sent }}</template><template v-else> · not sent (no channel on)</template></span>
      </div>
    </section>
  </div>
</template>

<style scoped>
h2 { font-size: 17px; font-weight: 600; margin: 0; }
.top { gap: 12px; padding: 16px 20px; }
.grow-card { flex: 1 1 420px; min-width: 0; }
.chan { border: 1px solid var(--card-line); border-radius: 10px; padding: 12px 14px 14px; margin: 0; display: flex; flex-direction: column; gap: 10px; }
.chan legend { padding: 0 6px; }
.fgrid { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 12px; }
.field { display: flex; flex-direction: column; gap: 6px; }
.field > span:first-child { font-size: 13px; font-weight: 500; color: var(--text-2); }
.in { min-height: 40px; padding: 0 10px; background: var(--sunken, #0F1318); border: 1px solid var(--btn-line, #2A3441); border-radius: 8px; color: var(--text); font-size: 13px; width: 100%; box-sizing: border-box; }
input[type=checkbox] { accent-color: var(--accent); width: 18px; height: 18px; }
.toggles { display: flex; flex-direction: column; gap: 8px; }
.tog { display: flex; align-items: center; gap: 10px; font-size: 14px; }
.note-row { display: flex; flex-direction: column; gap: 4px; padding: 10px 0; border-bottom: 1px solid var(--row-line); }
.text-2 { color: var(--text-2, #C5CEDA); }
.plain { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 6px; }
</style>
