<script setup lang="ts">
// DNS zone editor (Cloudflare / Hostinger), email DNS wizard, wildcard SSL.
interface Provider { id: string; kind: string; name: string; token_hint: string; created_at: string }
interface Rec { id: string; type: string; name: string; content: string; ttl: number; priority: number | null; proxied: boolean | null }

const props = defineProps<{ domains: string[] }>()
const api = useApi()
const providers = useResource<Provider[]>('/api/dns/providers')
const err = ref('')
const msg = ref('')
function fail(e: unknown) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) }

// ---- connect a provider
const np = reactive({ open: false, kind: 'cloudflare' as string | number | null, name: '', token: '', busy: false })
const kindOpts = [
  { value: 'cloudflare', label: 'Cloudflare', hint: 'API token with Zone → DNS → Edit' },
  { value: 'hostinger', label: 'Hostinger', hint: 'hPanel → API → new token' },
]
async function addProvider() {
  np.busy = true; err.value = ''
  try { await api.post('/api/dns/providers', { kind: np.kind, name: np.name.trim() || String(np.kind), token: np.token.trim() }); np.open = false; np.token = ''; np.name = ''; providers.refresh() } catch (e) { fail(e) } finally { np.busy = false }
}
async function removeProvider(p: Provider) {
  try { await api.del(`/api/dns/providers/${p.id}`); if (pid.value === p.id) pid.value = null; providers.refresh() } catch (e) { fail(e) }
}

// ---- zone
const pid = ref<string | number | null>(null)
watch(() => providers.data.value, (v) => { if (v?.length && !pid.value) pid.value = v[0]!.id }, { immediate: true })
const provider = computed(() => (providers.data.value ?? []).find((p) => p.id === pid.value))
const apexes = computed(() => [...new Set(props.domains.map((d) => d.split('.').slice(-2).join('.')))].sort())
const zone = ref<string | number | null>(null)
watch(apexes, (a) => { if (!zone.value && a.length) zone.value = a[0]! }, { immediate: true })
const recs = ref<Rec[]>([])
const loading = ref(false)
const q = ref('')
async function load() {
  if (!pid.value || !zone.value) return
  loading.value = true; err.value = ''
  try { recs.value = (await api.get<{ records: Rec[] }>(`/api/dns/${pid.value}/zones/${zone.value}/records`)).records } catch (e) { recs.value = []; fail(e) } finally { loading.value = false }
}
watch([pid, zone], load)
const shown = computed(() => recs.value.filter((r) => !q.value.trim() || `${r.type} ${r.name} ${r.content}`.toLowerCase().includes(q.value.trim().toLowerCase())))
const typeOpts = ['A', 'AAAA', 'CNAME', 'MX', 'TXT', 'NS', 'SRV', 'CAA']

// ---- add / edit
const ed = reactive({ open: false, id: '', type: 'A' as string | number | null, name: '@', content: '', ttl: 3600, priority: 10, proxied: false, busy: false })
function startAdd() { Object.assign(ed, { open: true, id: '', type: 'A', name: '@', content: '', ttl: provider.value?.kind === 'cloudflare' ? 1 : 3600, priority: 10, proxied: false }) }
function startEdit(r: Rec) { Object.assign(ed, { open: true, id: r.id, type: r.type, name: r.name, content: r.content, ttl: r.ttl, priority: r.priority ?? 10, proxied: !!r.proxied }) }
async function saveRec() {
  ed.busy = true; err.value = ''; msg.value = ''
  const body = { type: ed.type, name: ed.name.trim() || '@', content: ed.content.trim(), ttl: Number(ed.ttl) || 3600, priority: ed.type === 'MX' || ed.type === 'SRV' ? Number(ed.priority) : null, proxied: provider.value?.kind === 'cloudflare' && ['A', 'AAAA', 'CNAME'].includes(String(ed.type)) ? ed.proxied : null }
  try {
    if (ed.id) await api.put(`/api/dns/${pid.value}/zones/${zone.value}/records/${encodeURIComponent(ed.id)}`, body)
    else await api.post(`/api/dns/${pid.value}/zones/${zone.value}/records`, body)
    ed.open = false; msg.value = 'Saved — DNS changes can take a few minutes to show everywhere.'; await load()
  } catch (e) { fail(e) } finally { ed.busy = false }
}
async function delRec(r: Rec) {
  err.value = ''
  try { await api.del(`/api/dns/${pid.value}/zones/${zone.value}/records/${encodeURIComponent(r.id)}`); await load() } catch (e) { fail(e) }
}

// ---- email wizard
const mail = reactive({ open: false, provider: 'google' as string | number | null, mx: '', spf: '', dkimSel: '', dkimVal: '', dmarc: 'none' as string | number | null, dmarcEmail: '', preview: [] as Rec[], busy: false })
const mailOpts = [
  { value: 'google', label: 'Google Workspace' }, { value: 'microsoft', label: 'Microsoft 365' }, { value: 'zoho', label: 'Zoho Mail' },
  { value: 'fastmail', label: 'Fastmail' }, { value: 'custom', label: 'Other provider', hint: 'enter its MX hosts' },
]
const dmarcOpts = [{ value: 'none', label: 'Monitor only (p=none)' }, { value: 'quarantine', label: 'Send to spam (quarantine)' }, { value: 'reject', label: 'Reject' }]
function mailBody() {
  return { provider: mail.provider, mx: mail.mx.split('\n').map((l) => l.trim()).filter(Boolean), spf_include: mail.spf.trim() || null, dkim_selector: mail.dkimSel.trim() || null, dkim_value: mail.dkimVal.trim() || null, dmarc_policy: mail.dmarc, dmarc_email: mail.dmarcEmail.trim() || null }
}
async function previewMail() {
  err.value = ''
  try { mail.preview = (await api.post<{ records: Rec[] }>(`/api/dns/email/preview/${zone.value}`, mailBody())).records } catch (e) { fail(e) }
}
async function applyMail() {
  mail.busy = true; err.value = ''
  try { await api.post(`/api/dns/${pid.value}/zones/${zone.value}/email`, mailBody()); mail.open = false; msg.value = 'Email records saved (MX, SPF, DMARC' + (mail.dkimSel ? ', DKIM' : '') + ').'; await load() } catch (e) { fail(e) } finally { mail.busy = false }
}

// ---- wildcard SSL
const wc = reactive({ open: false, email: '', sites: [] as string[], pick: null as string | number | null, job: '' })
watch(() => wc.pick, (v) => { if (v && !wc.sites.includes(String(v))) wc.sites.push(String(v)); wc.pick = null })
const wcSiteOpts = computed(() => props.domains.filter((d) => zone.value && (d === zone.value || d.endsWith(`.${zone.value}`)) && d.split('.').length <= String(zone.value).split('.').length + 1 && !wc.sites.includes(d)))
async function wildcard() {
  err.value = ''
  try { wc.job = (await api.post<{ job_id: string }>(`/api/dns/${pid.value}/wildcard`, { zone: zone.value, email: wc.email.trim(), sites: wc.sites })).job_id } catch (e) { fail(e) }
}
</script>

<template>
  <section class="card" aria-labelledby="dns-h" style="overflow: hidden">
    <div style="display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px; padding: 16px 20px">
      <div style="display: flex; flex-direction: column; gap: 4px">
        <div class="row" style="gap: 10px"><AppIcon name="dns" :size="18" /><h2 id="dns-h" style="font-size: 17px; font-weight: 600">DNS zone editor</h2></div>
        <span class="small muted">Edit records where your domain's DNS lives (Cloudflare or Hostinger), set up email, get wildcard certificates.</span>
      </div>
      <div class="row" style="gap: 8px; flex-wrap: wrap">
        <div v-if="providers.data.value?.length" style="width: 200px"><SearchSelect v-model="pid" :options="(providers.data.value ?? []).map((p) => ({ value: p.id, label: p.name, hint: p.kind }))" placeholder="DNS account" aria-label="DNS account" /></div>
        <div v-if="providers.data.value?.length" style="width: 200px"><SearchSelect v-model="zone" :options="apexes" allow-custom mono placeholder="example.com" aria-label="Domain" /></div>
        <button type="button" class="btn btn-s" @click="np.open = !np.open"><AppIcon name="link" :size="14" /> Connect DNS account</button>
      </div>
    </div>

    <div v-if="np.open" style="border-top: 1px solid #1B222B; padding: 16px 20px; background: #0F1318; display: flex; flex-direction: column; gap: 10px">
      <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 10px">
        <label class="field"><span>Provider</span><SearchSelect v-model="np.kind" :options="kindOpts" placeholder="Choose" aria-label="Provider" /></label>
        <label class="field"><span>Name</span><input v-model="np.name" class="in" type="text" placeholder="My Cloudflare"></label>
        <label class="field"><span>API token</span><input v-model="np.token" class="in mono" type="password" placeholder="Paste the token" autocomplete="off"></label>
      </div>
      <p class="small muted">{{ np.kind === 'cloudflare' ? 'Cloudflare → My Profile → API Tokens → Create token → "Edit zone DNS" template.' : 'Hostinger hPanel → Account → API → Create token.' }} The token is checked now, stored on this server and never shown again.</p>
      <div class="row" style="gap: 8px"><button type="button" class="btn btn-p" :disabled="np.busy || np.token.trim().length < 20" @click="addProvider">{{ np.busy ? 'Checking…' : 'Connect' }}</button><button type="button" class="btn btn-s" @click="np.open = false">Cancel</button></div>
    </div>

    <p v-if="err" class="small" style="color: var(--bad); padding: 0 20px 12px" role="alert">{{ err }}</p>
    <p v-else-if="msg" class="small" style="color: var(--ok); padding: 0 20px 12px" role="status">{{ msg }}</p>

    <div v-if="!providers.data.value?.length && !np.open" style="padding: 0 20px 18px" class="small muted">No DNS account connected. Without one, NexaCloud still checks where each domain points (above) — connect Cloudflare or Hostinger to edit records here.</div>

    <template v-if="provider && zone">
      <div class="row" style="gap: 8px; padding: 0 20px 12px; flex-wrap: wrap">
        <input v-model="q" class="in" type="search" placeholder="Filter records" aria-label="Filter records" style="width: 200px">
        <button type="button" class="btn btn-p btn-s" @click="startAdd"><AppIcon name="plus" :size="14" /> Add record</button>
        <button type="button" class="btn btn-s" @click="mail.open = !mail.open; mail.preview = []"><AppIcon name="mail" :size="14" /> Set up email</button>
        <button type="button" class="btn btn-s" :disabled="provider.kind !== 'cloudflare'" :title="provider.kind !== 'cloudflare' ? 'Needs a Cloudflare DNS account' : undefined" @click="wc.open = !wc.open"><AppIcon name="ssl" :size="14" /> Wildcard SSL</button>
        <ConfirmAction label="Disconnect account" confirm-label="Disconnect" question="Remove this DNS account from NexaCloud? Records stay at the provider." @confirm="removeProvider(provider)" />
      </div>

      <div v-if="ed.open" style="border-top: 1px solid #1B222B; padding: 14px 20px; background: #0F1318; display: flex; flex-direction: column; gap: 10px">
        <div style="display: grid; grid-template-columns: 110px minmax(120px, 1fr) minmax(200px, 2fr) 100px; gap: 8px">
          <SearchSelect v-model="ed.type" :options="typeOpts" placeholder="Type" aria-label="Record type" />
          <input v-model="ed.name" class="in mono" type="text" placeholder="@ or www" aria-label="Name">
          <input v-model="ed.content" class="in mono" type="text" :placeholder="ed.type === 'A' ? '203.0.113.10' : ed.type === 'CNAME' ? 'target.example.com' : ed.type === 'MX' ? 'mx1.example.net' : 'value'" aria-label="Value">
          <input v-model.number="ed.ttl" class="in mono" type="number" min="1" placeholder="TTL" aria-label="TTL">
        </div>
        <div class="row" style="gap: 14px; flex-wrap: wrap">
          <label v-if="ed.type === 'MX' || ed.type === 'SRV'" class="row small" style="gap: 6px">Priority <input v-model.number="ed.priority" class="in mono" type="number" min="0" max="65535" placeholder="10" style="width: 80px"></label>
          <label v-if="provider.kind === 'cloudflare' && ['A', 'AAAA', 'CNAME'].includes(String(ed.type))" class="row small" style="gap: 6px"><input v-model="ed.proxied" type="checkbox"> Proxied through Cloudflare (orange cloud)</label>
          <span class="small muted">{{ zone }} · name "{{ ed.name || '@' }}" = {{ (ed.name || '@') === '@' ? zone : `${ed.name}.${zone}` }}</span>
        </div>
        <div class="row" style="gap: 8px"><button type="button" class="btn btn-p" :disabled="ed.busy || !ed.content.trim()" @click="saveRec">{{ ed.busy ? 'Saving…' : ed.id ? 'Save record' : 'Add record' }}</button><button type="button" class="btn btn-s" @click="ed.open = false">Cancel</button></div>
      </div>

      <div v-if="mail.open" style="border-top: 1px solid #1B222B; padding: 14px 20px; background: #0F1318; display: flex; flex-direction: column; gap: 10px">
        <p class="small muted">Point {{ zone }}'s email at your mail provider. This replaces the MX records, the SPF record and the DMARC record; other records stay.</p>
        <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 10px">
          <label class="field"><span>Mail provider</span><SearchSelect v-model="mail.provider" :options="mailOpts" placeholder="Choose" aria-label="Mail provider" /></label>
          <label class="field"><span>DMARC</span><SearchSelect v-model="mail.dmarc" :options="dmarcOpts" placeholder="Choose" aria-label="DMARC policy" /></label>
          <label class="field"><span>DMARC reports to (optional)</span><input v-model="mail.dmarcEmail" class="in" type="email" placeholder="dmarc@example.com"></label>
        </div>
        <div v-if="mail.provider === 'custom'" style="display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 10px">
          <label class="field"><span>MX hosts (one per line: priority host)</span><textarea v-model="mail.mx" class="in mono" rows="3" placeholder="10 mx1.mailhost.net&#10;20 mx2.mailhost.net" style="min-height: 70px; padding: 8px" /></label>
          <label class="field"><span>SPF include</span><input v-model="mail.spf" class="in mono" type="text" placeholder="spf.mailhost.net"></label>
        </div>
        <div style="display: grid; grid-template-columns: 160px 1fr; gap: 10px">
          <label class="field"><span>DKIM selector</span><input v-model="mail.dkimSel" class="in mono" type="text" placeholder="google"></label>
          <label class="field"><span>DKIM value (from your mail provider)</span><input v-model="mail.dkimVal" class="in mono" type="text" placeholder="v=DKIM1; k=rsa; p=MIIB…"></label>
        </div>
        <div v-if="mail.preview.length" class="mono" style="background: #07090C; border: 1px solid #1B222B; border-radius: 8px; padding: 10px 12px; font-size: 12px; line-height: 1.7; white-space: pre-wrap">{{ mail.preview.map((r) => `${r.type.padEnd(5)} ${r.name.padEnd(22)} ${r.priority !== null ? r.priority + ' ' : ''}${r.content}`).join('\n') }}</div>
        <div class="row" style="gap: 8px"><button type="button" class="btn btn-s" @click="previewMail">Preview records</button><button type="button" class="btn btn-p" :disabled="mail.busy" @click="applyMail">{{ mail.busy ? 'Saving…' : 'Save email records' }}</button><button type="button" class="btn btn-s" @click="mail.open = false">Cancel</button></div>
        <p class="small muted">Email forwarding (info@ → your inbox) is set up at your mail provider or in Cloudflare Email Routing — NexaCloud doesn't run a mail server.</p>
      </div>

      <div v-if="wc.open" style="border-top: 1px solid #1B222B; padding: 14px 20px; background: #0F1318; display: flex; flex-direction: column; gap: 10px">
        <p class="small muted">Gets one Let's Encrypt certificate for <span class="mono">{{ zone }}</span> and <span class="mono">*.{{ zone }}</span> by proving ownership through Cloudflare DNS (no need for the sites to be reachable yet). Renewed automatically.</p>
        <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 10px">
          <label class="field"><span>Email for expiry notices</span><input v-model="wc.email" class="in" type="email" placeholder="you@example.com"></label>
          <label class="field"><span>Use it for these sites (optional)</span><SearchSelect v-model="wc.pick" :options="wcSiteOpts" placeholder="Add a site…" aria-label="Add site" /></label>
        </div>
        <div class="row" style="gap: 6px; flex-wrap: wrap"><span v-for="s in wc.sites" :key="s" class="pill mono">{{ s }} <button type="button" style="background: none; border: 0; color: inherit; cursor: pointer" :aria-label="`Remove ${s}`" @click="wc.sites = wc.sites.filter((x) => x !== s)">✕</button></span></div>
        <JobLog v-if="wc.job" :key="wc.job" :job-id="wc.job" status="running" />
        <div class="row" style="gap: 8px"><button type="button" class="btn btn-p" :disabled="!/.+@.+\..+/.test(wc.email)" @click="wildcard">Get wildcard certificate</button><button type="button" class="btn btn-s" @click="wc.open = false">Close</button></div>
      </div>

      <div style="overflow-x: auto">
        <table>
          <thead><tr><th>Type</th><th>Name</th><th>Value</th><th>TTL</th><th><span class="sr-only">Actions</span></th></tr></thead>
          <tbody>
            <tr v-for="r in shown" :key="r.id">
              <td><span class="pill mono">{{ r.type }}</span></td>
              <td class="mono" style="font-size: 13px">{{ r.name }}</td>
              <td class="mono" style="font-size: 13px; max-width: 420px; white-space: normal; word-break: break-all">{{ r.priority !== null ? `${r.priority} ` : '' }}{{ r.content }}<span v-if="r.proxied" class="pill" style="margin-left: 6px">proxied</span></td>
              <td class="mono" style="font-size: 13px">{{ r.ttl === 1 ? 'auto' : r.ttl }}</td>
              <td><div class="row" style="gap: 6px"><button type="button" class="btn btn-s" :aria-label="`Edit ${r.type} ${r.name}`" @click="startEdit(r)"><AppIcon name="edit" :size="13" /></button><ConfirmAction label="Delete" confirm-label="Delete record" @confirm="delRec(r)" /></div></td>
            </tr>
            <tr v-if="!shown.length"><td colspan="5" style="color: #A3AEBD">{{ loading ? 'Loading…' : 'No records (or the zone is not in this account).' }}</td></tr>
          </tbody>
        </table>
      </div>
    </template>
  </section>
</template>

<style scoped>
.field { display: flex; flex-direction: column; gap: 6px; }
.field > span:first-child { font-size: 13px; font-weight: 500; color: var(--text-2, #C5CEDA); }
</style>
