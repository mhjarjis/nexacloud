<script setup lang="ts">
useHead({ title: 'Server · NexaCloud' })

interface Info { timezone: string; reboot_required: boolean; reboot_packages: string[]; uptime_secs: number | null; kernel: string; last_apt_update: number | null; auto_security_updates: boolean }
interface Upd { name: string; from: string; to: string; security: boolean }
interface Keys { keys: { fingerprint: string; kind: string; comment: string; bits: string }[]; file: string }

const api = useApi()
const info = useResource<Info>('/api/server')
const sys = useResource<{ hostname: string }>('/api/system')
const upd = useResource<{ packages: Upd[]; security: number; checked_at: number | null }>('/api/server/updates')
const keys = useResource<Keys>('/api/server/ssh-keys')
const tzList = ref<string[]>([])
onMounted(async () => { try { tzList.value = await api.get<string[]>('/api/server/timezones') } catch { tzList.value = [] } })
const host = computed(() => sys.data.value?.hostname ?? 'server')
const err = ref('')
const msg = ref('')

function fmtUptime(s: number | null | undefined) {
  if (!s) return '—'
  const d = Math.floor(s / 86400); const h = Math.floor((s % 86400) / 3600)
  return d ? `${d} d ${h} h` : `${h} h ${Math.floor((s % 3600) / 60)} min`
}
const stats = computed(() => [
  { k: 'Uptime', v: fmtUptime(info.data.value?.uptime_secs) },
  { k: 'Updates waiting', v: upd.data.value ? String(upd.data.value.packages.length) : '—' },
  { k: 'Security updates', v: upd.data.value ? String(upd.data.value.security) : '—' },
  { k: 'Timezone', v: info.data.value?.timezone ?? '—' },
])

// ---- updates
const updJob = ref<string | null>(null)
const updQ = ref('')
const shownUpd = computed(() => (upd.data.value?.packages ?? []).filter((p) => !updQ.value.trim() || p.name.includes(updQ.value.trim())))
async function startJob(path: string, body?: Record<string, unknown>) {
  err.value = ''; msg.value = ''
  try { updJob.value = (await api.post<{ job_id: string }>(path, body)).job_id } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) }
}
function jobDone() { upd.refresh(); info.refresh() }

// ---- timezone
const tz = ref<string | number | null>(null)
watch(() => info.data.value?.timezone, (v) => { if (v && tz.value === null) tz.value = v }, { immediate: true })
const tzBusy = ref(false)
async function saveTz() {
  tzBusy.value = true; err.value = ''; msg.value = ''
  try { await api.put('/api/server/timezone', { timezone: tz.value }); msg.value = `Timezone set to ${tz.value}.`; info.refresh() } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) } finally { tzBusy.value = false }
}

// ---- SSH keys
const newKey = ref('')
async function addKey() {
  err.value = ''; msg.value = ''
  try { await api.post('/api/server/ssh-keys', { key: newKey.value.trim() }); newKey.value = ''; msg.value = 'Key added.'; keys.refresh() } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) }
}
async function removeKey(fp: string) {
  err.value = ''; msg.value = ''
  try { await api.del(`/api/server/ssh-keys/${encodeURIComponent(fp)}`); msg.value = 'Key removed.'; keys.refresh() } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) }
}

// ---- reboot
const rb = reactive({ open: false, confirm: '', busy: false, done: '' })
async function reboot() {
  rb.busy = true; err.value = ''
  try { const r = await api.post<{ dry_run?: boolean }>('/api/server/reboot', { confirm: rb.confirm.trim() }); rb.done = r.dry_run ? 'Test mode: reboot skipped.' : 'Rebooting — the panel will be back in about a minute.' } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) } finally { rb.busy = false }
}
</script>

<template>
  <header style="display: flex; flex-wrap: wrap; align-items: flex-end; justify-content: space-between; gap: 16px">
    <div style="display: flex; flex-direction: column; gap: 6px">
      <p class="lbl">{{ host }} / Server</p>
      <h1 style="font-size: 30px; font-weight: 600">Server</h1>
    </div>
    <span v-if="info.data.value?.reboot_required" class="pill warn">Reboot needed{{ info.data.value.reboot_packages.length ? ` (${info.data.value.reboot_packages.slice(0, 3).join(', ')})` : '' }}</span>
  </header>

  <section aria-label="Server facts" style="display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 12px">
    <div v-for="s in stats" :key="s.k" class="card" style="padding: 16px; display: flex; flex-direction: column; gap: 6px">
      <span class="lbl">{{ s.k }}</span>
      <span style="font-family: 'Space Grotesk', sans-serif; font-size: 22px; font-weight: 600; overflow-wrap: anywhere">{{ s.v }}</span>
    </div>
  </section>

  <p v-if="err" class="small" style="color: var(--bad)" role="alert">{{ err }}</p>
  <p v-else-if="msg" class="small" style="color: var(--ok)" role="status">{{ msg }}</p>

  <section class="card" aria-labelledby="upd-h" style="overflow: hidden">
    <div style="display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px; padding: 16px 20px">
      <div style="display: flex; flex-direction: column; gap: 4px">
        <div style="display: flex; align-items: center; gap: 10px"><AppIcon name="download" :size="18" /><h2 id="upd-h" style="font-size: 17px; font-weight: 600">Package updates</h2></div>
        <span style="font-size: 12px; color: #A3AEBD">Last checked {{ upd.data.value?.checked_at ? formatRelative(new Date(upd.data.value.checked_at * 1000).toISOString()) : 'never' }} · automatic security updates {{ info.data.value?.auto_security_updates ? 'on' : 'off (turn on in Advisor)' }}</span>
      </div>
      <div style="display: flex; flex-wrap: wrap; gap: 8px">
        <input v-model="updQ" class="in" type="search" placeholder="Filter packages" aria-label="Filter packages" style="width: 180px">
        <button type="button" class="btn btn-s" @click="startJob('/api/server/updates/check')"><AppIcon name="refresh" :size="14" /> Check now</button>
        <button type="button" class="btn btn-s" :disabled="!upd.data.value?.security" @click="startJob('/api/server/updates/apply', { security_only: true })"><AppIcon name="shield" :size="14" /> Install security updates</button>
        <button type="button" class="btn btn-p" :disabled="!upd.data.value?.packages.length" @click="startJob('/api/server/updates/apply', { security_only: false })">Install all updates</button>
      </div>
    </div>
    <div v-if="updJob" style="padding: 0 20px 16px"><JobLog :key="updJob" :job-id="updJob" status="running" @finished="jobDone" /></div>
    <div style="overflow-x: auto; max-height: 420px">
      <table>
        <thead><tr><th>Package</th><th>Installed</th><th>New</th><th>Kind</th></tr></thead>
        <tbody>
          <tr v-for="p in shownUpd" :key="p.name">
            <td class="mono" style="font-weight: 500">{{ p.name }}</td>
            <td class="mono" style="font-size: 13px; color: #A3AEBD">{{ p.from }}</td>
            <td class="mono" style="font-size: 13px">{{ p.to }}</td>
            <td><span :class="`pill ${p.security ? 'warn' : ''}`">{{ p.security ? 'security' : 'update' }}</span></td>
          </tr>
          <tr v-if="!shownUpd.length"><td colspan="4" style="color: #A3AEBD">{{ upd.loading.value ? 'Loading…' : 'Everything is up to date (press Check now to refresh the list).' }}</td></tr>
        </tbody>
      </table>
    </div>
  </section>

  <div style="display: flex; flex-wrap: wrap; gap: 16px; align-items: flex-start">
    <section class="card" aria-labelledby="tz-h" style="flex: 1 1 320px; min-width: 0; padding: 20px; display: flex; flex-direction: column; gap: 12px">
      <div style="display: flex; align-items: center; gap: 10px"><AppIcon name="clock" :size="18" /><h2 id="tz-h" style="font-size: 17px; font-weight: 600">Timezone</h2></div>
      <p style="font-size: 13px; color: #A3AEBD">Used by cron jobs, backups at "03:00" and log times.</p>
      <SearchSelect v-model="tz" :options="tzList.length ? tzList : [info.data.value?.timezone ?? 'UTC']" placeholder="Search a timezone (e.g. Europe/Amsterdam)" aria-label="Timezone" />
      <button type="button" class="btn btn-p" style="align-self: flex-start" :disabled="tzBusy || !tz || tz === info.data.value?.timezone" @click="saveTz">{{ tzBusy ? 'Saving…' : 'Set timezone' }}</button>
    </section>

    <section class="card" aria-labelledby="rb-h" style="flex: 1 1 320px; min-width: 0; padding: 20px; display: flex; flex-direction: column; gap: 12px">
      <div style="display: flex; align-items: center; gap: 10px"><AppIcon name="restart" :size="18" /><h2 id="rb-h" style="font-size: 17px; font-weight: 600">Reboot</h2></div>
      <p style="font-size: 13px; color: #A3AEBD">Restarts the whole server. Sites are down for about a minute. {{ info.data.value?.reboot_required ? 'A reboot is needed to finish installed updates.' : '' }}</p>
      <template v-if="rb.done"><span class="pill ok" style="align-self: flex-start">{{ rb.done }}</span></template>
      <template v-else-if="rb.open">
        <label style="display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Type <span class="mono">{{ host }}</span> to confirm</span><input v-model="rb.confirm" class="in mono" type="text" :placeholder="host" autocomplete="off" spellcheck="false"></label>
        <div style="display: flex; gap: 8px"><button type="button" class="btn btn-danger" :disabled="rb.busy || rb.confirm.trim() !== host" @click="reboot">Reboot now</button><button type="button" class="btn btn-s" @click="rb.open = false">Cancel</button></div>
      </template>
      <button v-else type="button" class="btn btn-s" style="align-self: flex-start" @click="rb.open = true">Reboot server…</button>
    </section>
  </div>

  <section class="card" aria-labelledby="key-h" style="overflow: hidden">
    <div style="display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px; padding: 16px 20px">
      <div style="display: flex; align-items: center; gap: 10px"><AppIcon name="key" :size="18" /><h2 id="key-h" style="font-size: 17px; font-weight: 600">SSH keys for root · {{ keys.data.value?.keys.length ?? 0 }}</h2></div>
      <span class="mono" style="font-size: 12px; color: #A3AEBD">{{ keys.data.value?.file }}</span>
    </div>
    <div style="overflow-x: auto">
      <table>
        <thead><tr><th>Key</th><th>Fingerprint</th><th>Comment</th><th><span style="position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0)">Actions</span></th></tr></thead>
        <tbody>
          <tr v-for="k in keys.data.value?.keys ?? []" :key="k.fingerprint">
            <td class="mono" style="font-size: 13px">{{ k.kind }} · {{ k.bits }}</td>
            <td class="mono" style="font-size: 12px">{{ k.fingerprint }}</td>
            <td style="font-size: 13px; color: #C5CEDA">{{ k.comment || '—' }}</td>
            <td><ConfirmAction label="Remove" confirm-label="Remove key" question="Remove this key? Make sure you have another way in." :disabled="(keys.data.value?.keys.length ?? 0) <= 1" @confirm="removeKey(k.fingerprint)" /></td>
          </tr>
          <tr v-if="!(keys.data.value?.keys ?? []).length"><td colspan="4" style="color: #A3AEBD">No SSH keys for root.</td></tr>
        </tbody>
      </table>
    </div>
    <div style="border-top: 1px solid #1B222B; padding: 16px 20px; display: flex; flex-wrap: wrap; gap: 8px; align-items: flex-end">
      <label style="flex: 1 1 360px; display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Add a public key</span><input v-model="newKey" class="in mono" type="text" placeholder="ssh-ed25519 AAAA… you@laptop" spellcheck="false"></label>
      <button type="button" class="btn btn-p" :disabled="!newKey.trim()" @click="addKey"><AppIcon name="plus" :size="14" /> Add key</button>
    </div>
  </section>
</template>
