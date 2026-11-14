<script setup lang="ts">
// Per-site security & tuning: blocked IPs, password-protected folders,
// custom error pages, PHP pool tuning, own SSL certificate, SFTP accounts.
interface Protected { path: string; realm: string }
interface ErrPage { code: number; path: string }
interface Pool { pm: string; max_children: number; memory_limit: string; upload_max_filesize: string; max_execution_time: number }
interface Model {
  id: string
  kind: string
  user: string | null
  php_version: string | null
  ssl: boolean
  ssl_cert: string | null
  pool: Pool | null
  options: Record<string, unknown> & { deny_ips?: string[]; protected?: Protected[]; error_pages?: ErrPage[] }
}
interface SftpUser { username: string; site_id: string; dir: string; created_at: string; has_key: boolean; mounted: boolean }

const props = defineProps<{ siteId: string; isAdmin?: boolean }>()
const emit = defineEmits<{ changed: [] }>()
const api = useApi()
const model = ref<Model | null>(null)
const err = ref('')
const ok = ref('')
const busy = ref('')

async function load() {
  try { model.value = await api.get<Model>(`/api/sites/${encodeURIComponent(props.siteId)}/model`) } catch (e) { err.value = errorMessage(e) }
  try { authUsers.value = (await api.get<{ users: string[] }>(`/api/sites/${encodeURIComponent(props.siteId)}/auth-users`)).users } catch { authUsers.value = [] }
  if (props.isAdmin) {
    try { sftp.value = (await api.get<SftpUser[]>('/api/sftp')).filter((u) => u.site_id === props.siteId) } catch { sftp.value = [] }
  }
}
watch(() => props.siteId, load, { immediate: true })
watch(model, (m) => {
  if (!m) return
  ipsText.value = (m.options.deny_ips ?? []).join('\n')
  prot.value = (m.options.protected ?? []).map((p) => ({ ...p }))
  pages.value = (m.options.error_pages ?? []).map((p) => ({ ...p }))
  pool.value = { ...(m.pool ?? { pm: 'ondemand', max_children: 10, memory_limit: '256M', upload_max_filesize: '64M', max_execution_time: 60 }) }
})

async function quickPlan(request: { kind: string; params: Record<string, unknown> }): Promise<string> {
  const pv = await api.post<{ plan_hash: string }>('/api/plans/preview', request)
  const started = await api.post<{ change_id: string }>('/api/plans/apply', { ...request, plan_hash: pv.plan_hash })
  for (let i = 0; i < 180; i++) {
    const c = await api.get<{ status: string; error?: string | null }>(`/api/changes/${started.change_id}`)
    if (['applied', 'reverted', 'failed', 'revert_failed'].includes(c.status)) {
      if (c.status !== 'applied' && c.error) throw new Error(`${c.status}: ${c.error}`)
      return c.status
    }
    await new Promise((r) => setTimeout(r, 1000))
  }
  return 'timeout'
}
async function run(key: string, fn: () => Promise<unknown>, done: string) {
  busy.value = key; err.value = ''; ok.value = ''
  try {
    const r = await fn()
    if (typeof r === 'string' && r !== 'applied') throw new Error(`Change ${r} — see Activity.`)
    ok.value = done
    await load()
    emit('changed')
  } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) } finally { busy.value = '' }
}
function saveOptions(patch: Record<string, unknown>, done: string, key: string) {
  if (!model.value) return
  return run(key, () => quickPlan({ kind: 'site_options', params: { id: props.siteId, options: { ...model.value!.options, ...patch } } }), done)
}

// ---- blocked IPs
const ipsText = ref('')
const ipList = computed(() => ipsText.value.split(/[\s,]+/).map((s) => s.trim()).filter(Boolean))

// ---- protected folders
const prot = ref<Protected[]>([])
const authUsers = ref<string[]>([])
const newUser = reactive({ name: '', password: '' })
function addUser() {
  const n = newUser.name.trim(); if (!n || !newUser.password) return
  const users = [...authUsers.value.filter((u) => u !== n).map((u) => ({ name: u })), { name: n, password: newUser.password }]
  run('users', () => api.put(`/api/sites/${encodeURIComponent(props.siteId)}/auth-users`, { users }), `Saved ${n}.`).then(() => { newUser.name = ''; newUser.password = '' })
}
function removeUser(n: string) {
  run('users', () => api.put(`/api/sites/${encodeURIComponent(props.siteId)}/auth-users`, { users: authUsers.value.filter((u) => u !== n).map((u) => ({ name: u })) }), `Removed ${n}.`)
}

// ---- error pages
const pages = ref<ErrPage[]>([])
const codeOpts = [403, 404, 500, 502, 503, 504].map((c) => ({ value: c, label: String(c), hint: { 403: 'forbidden', 404: 'not found', 500: 'server error', 502: 'PHP down', 503: 'unavailable', 504: 'timeout' }[c] }))

// ---- pool
const pool = ref<Pool>({ pm: 'ondemand', max_children: 10, memory_limit: '256M', upload_max_filesize: '64M', max_execution_time: 60 })
const pmOpts = [
  { value: 'ondemand', label: 'On demand', hint: 'start workers when needed — least memory' },
  { value: 'dynamic', label: 'Dynamic', hint: 'keep a few workers warm' },
  { value: 'static', label: 'Static', hint: 'always run max workers — busiest sites' },
]

// ---- own certificate
const cert = reactive({ open: false, cert: '', key: '' })

// ---- SFTP
const sftp = ref<SftpUser[]>([])
const ns = reactive({ username: '', password: '', key: '', open: false })
function genPw() { const a = new Uint8Array(18); crypto.getRandomValues(a); ns.password = btoa(String.fromCharCode(...a)).replace(/[+/=]/g, '').slice(0, 20) }
const host = computed(() => (typeof window !== 'undefined' ? window.location.hostname : 'server'))
</script>

<template>
  <div class="stack" style="gap: 16px">
    <p v-if="err" class="small" style="color: var(--bad)" role="alert">{{ err }}</p>
    <p v-else-if="ok" class="small" style="color: var(--ok)" role="status">{{ ok }}</p>

    <div class="split">
      <!-- Blocked IPs -->
      <section class="card card-pad stack set-card" aria-labelledby="sec-ip">
        <div class="row" style="gap: 10px"><AppIcon name="shield" :size="18" /><h2 id="sec-ip">Blocked IP addresses</h2></div>
        <p class="small muted">Visitors from these addresses get "403 Forbidden" on this site. One per line; ranges like <span class="mono">203.0.113.0/24</span> work too. To block an address on every site, use Security → Block IPs.</p>
        <textarea v-model="ipsText" class="in mono" rows="4" placeholder="203.0.113.7&#10;198.51.100.0/24" aria-label="Blocked IP addresses" style="min-height: 96px; padding: 10px; resize: vertical" />
        <button type="button" class="btn btn-p" style="align-self: flex-start" :disabled="!!busy || !model" @click="saveOptions({ deny_ips: ipList }, ipList.length ? `${ipList.length} address(es) blocked.` : 'No addresses blocked.', 'ips')">{{ busy === 'ips' ? 'Saving…' : 'Save blocked IPs' }}</button>
      </section>

      <!-- Error pages -->
      <section class="card card-pad stack set-card" aria-labelledby="sec-err">
        <div class="row" style="gap: 10px"><AppIcon name="file" :size="18" /><h2 id="sec-err">Custom error pages</h2></div>
        <p class="small muted">Show your own page when nginx itself returns an error — for example 502 while PHP restarts. The path is a file inside the web root.</p>
        <div v-for="(pg, i) in pages" :key="i" style="display: grid; grid-template-columns: 110px 1fr auto; gap: 8px">
          <SearchSelect v-model="pg.code" :options="codeOpts" placeholder="Code" aria-label="Error code" />
          <input v-model="pg.path" class="in mono" type="text" :placeholder="`/errors/${pg.code}.html`" aria-label="Page path">
          <button type="button" class="btn btn-s" :aria-label="`Remove ${pg.code}`" @click="pages.splice(i, 1)"><AppIcon name="trash" :size="14" /></button>
        </div>
        <div class="row" style="gap: 8px">
          <button type="button" class="btn btn-s" @click="pages.push({ code: 502, path: '/errors/502.html' })"><AppIcon name="plus" :size="14" /> Add page</button>
          <button type="button" class="btn btn-p" :disabled="!!busy || !model" @click="saveOptions({ error_pages: pages.map((p) => ({ code: Number(p.code), path: p.path.trim() })) }, 'Error pages saved.', 'pages')">{{ busy === 'pages' ? 'Saving…' : 'Save error pages' }}</button>
        </div>
      </section>
    </div>

    <!-- Protected folders -->
    <section class="card card-pad stack" aria-labelledby="sec-prot">
      <div class="row" style="gap: 10px"><AppIcon name="lock" :size="18" /><h2 id="sec-prot">Password-protected folders</h2></div>
      <p class="small muted">The browser asks for a username and password before showing these paths. Use <span class="mono">/</span> to protect the whole site (handy for staging). The same users work for every protected folder of this site.</p>
      <div style="display: flex; flex-wrap: wrap; gap: 16px">
        <div class="stack" style="flex: 1 1 320px; gap: 8px">
          <span class="small" style="font-weight: 600">Folders</span>
          <div v-for="(d, i) in prot" :key="i" style="display: grid; grid-template-columns: 1fr 1fr auto; gap: 8px">
            <input v-model="d.path" class="in mono" type="text" placeholder="/admin" aria-label="Folder path">
            <input v-model="d.realm" class="in" type="text" placeholder="Restricted" aria-label="Prompt text">
            <button type="button" class="btn btn-s" :aria-label="`Remove ${d.path}`" @click="prot.splice(i, 1)"><AppIcon name="trash" :size="14" /></button>
          </div>
          <p v-if="!prot.length" class="small muted">No protected folders.</p>
          <div class="row" style="gap: 8px">
            <button type="button" class="btn btn-s" @click="prot.push({ path: '/admin', realm: 'Restricted' })"><AppIcon name="plus" :size="14" /> Add folder</button>
            <button type="button" class="btn btn-p" :disabled="!!busy || !model || (prot.length > 0 && !authUsers.length)" :title="prot.length && !authUsers.length ? 'Add a user first' : undefined" @click="saveOptions({ protected: prot.map((p) => ({ path: p.path.trim(), realm: p.realm.trim() || 'Restricted' })) }, 'Protected folders saved.', 'prot')">{{ busy === 'prot' ? 'Saving…' : 'Save folders' }}</button>
          </div>
        </div>
        <div class="stack" style="flex: 1 1 320px; gap: 8px">
          <span class="small" style="font-weight: 600">Users · {{ authUsers.length }}</span>
          <div class="row" style="gap: 6px; flex-wrap: wrap">
            <span v-for="u in authUsers" :key="u" class="pill mono">{{ u }} <button type="button" style="background: none; border: 0; color: inherit; cursor: pointer; padding: 0 0 0 4px" :aria-label="`Remove ${u}`" @click="removeUser(u)">✕</button></span>
          </div>
          <div style="display: grid; grid-template-columns: 1fr 1fr auto; gap: 8px">
            <input v-model="newUser.name" class="in mono" type="text" placeholder="client" aria-label="Username" autocomplete="off">
            <input v-model="newUser.password" class="in" type="text" placeholder="Password (8+ characters)" aria-label="Password" autocomplete="off">
            <button type="button" class="btn btn-s" :disabled="!!busy || !newUser.name.trim() || newUser.password.length < 8" @click="addUser">Add</button>
          </div>
          <p class="small muted">Adding an existing name changes its password.</p>
        </div>
      </div>
    </section>

    <div class="split">
      <!-- PHP pool -->
      <section v-if="model?.php_version" class="card card-pad stack set-card" aria-labelledby="sec-pool">
        <div class="row" style="gap: 10px"><AppIcon name="gauge" :size="18" /><h2 id="sec-pool">PHP workers &amp; limits</h2></div>
        <template v-if="model.user">
          <p class="small muted">This site has its own PHP pool (user <span class="mono">{{ model.user }}</span>). Changes are checked with php-fpm and rolled back if they fail.</p>
          <div class="fgrid">
            <label class="field"><span>Process manager</span><SearchSelect v-model="pool.pm" :options="pmOpts" placeholder="Choose" aria-label="Process manager" /></label>
            <label class="field"><span>Max workers</span><input v-model.number="pool.max_children" class="in mono" type="number" min="1" max="200" placeholder="10"></label>
            <label class="field"><span>Memory per request</span><input v-model="pool.memory_limit" class="in mono" type="text" placeholder="256M"></label>
            <label class="field"><span>Max upload size</span><input v-model="pool.upload_max_filesize" class="in mono" type="text" placeholder="64M"></label>
            <label class="field"><span>Max time per request (s)</span><input v-model.number="pool.max_execution_time" class="in mono" type="number" min="5" max="3600" placeholder="60"></label>
          </div>
          <button type="button" class="btn btn-p" style="align-self: flex-start" :disabled="!!busy" @click="run('pool', () => quickPlan({ kind: 'site_pool', params: { id: siteId, pool } }), 'PHP pool updated.')">{{ busy === 'pool' ? 'Applying…' : 'Apply PHP limits' }}</button>
        </template>
        <p v-else class="small muted">This site shares the server's PHP pool. Give it its own user under "Domains, HTTPS, runtime &amp; nginx" → Isolate, then tune its workers and limits here.</p>
      </section>

      <!-- Own certificate -->
      <section class="card card-pad stack set-card" aria-labelledby="sec-cert">
        <div class="row" style="gap: 10px"><AppIcon name="ssl" :size="18" /><h2 id="sec-cert">Your own SSL certificate</h2></div>
        <p class="small muted">Bought a certificate (or need a wildcard)? Paste it here instead of using Let's Encrypt. <template v-if="model?.ssl_cert?.startsWith('/etc/nexacloud/ssl/')">This site already uses an uploaded certificate.</template></p>
        <template v-if="cert.open">
          <label class="field"><span>Certificate (with intermediates)</span><textarea v-model="cert.cert" class="in mono" rows="5" placeholder="-----BEGIN CERTIFICATE-----&#10;…&#10;-----END CERTIFICATE-----" style="min-height: 110px; padding: 10px; font-size: 12px" /></label>
          <label class="field"><span>Private key</span><textarea v-model="cert.key" class="in mono" rows="4" placeholder="-----BEGIN PRIVATE KEY-----&#10;…&#10;-----END PRIVATE KEY-----" style="min-height: 90px; padding: 10px; font-size: 12px" autocomplete="off" /></label>
          <p class="small muted">The key is stored readable by root only and is never shown again (it's masked in Activity).</p>
          <div class="row" style="gap: 8px">
            <button type="button" class="btn btn-p" :disabled="!!busy || !cert.cert.trim() || !cert.key.trim()" @click="run('cert', () => quickPlan({ kind: 'site_ssl_custom', params: { id: siteId, cert: cert.cert, key: cert.key } }), 'Your certificate is live.').then(() => { if (!err) { cert.open = false; cert.cert = ''; cert.key = '' } })">{{ busy === 'cert' ? 'Checking…' : 'Install certificate' }}</button>
            <button type="button" class="btn btn-s" @click="cert.open = false">Cancel</button>
          </div>
        </template>
        <button v-else type="button" class="btn btn-s" style="align-self: flex-start" @click="cert.open = true"><AppIcon name="upload" :size="14" /> Upload certificate</button>
      </section>
    </div>

    <!-- SFTP -->
    <section v-if="isAdmin" class="card" aria-labelledby="sec-sftp" style="overflow: hidden">
      <div class="row" style="justify-content: space-between; gap: 10px; padding: 16px 20px">
        <div class="row" style="gap: 10px"><AppIcon name="folder" :size="18" /><h2 id="sec-sftp">SFTP accounts · {{ sftp.length }}</h2></div>
        <button type="button" class="btn btn-p" @click="ns.open = !ns.open; if (ns.open && !ns.password) genPw()"><AppIcon name="plus" :size="14" /> Add SFTP account</button>
      </div>
      <div v-if="ns.open" class="stack" style="gap: 10px; padding: 16px 20px; background: #0F1318; border-top: 1px solid #1B222B">
        <p class="small muted">The account only sees this site's folder (as <span class="mono">/site</span>), can't open a shell, and files it uploads belong to the site's user.</p>
        <div class="fgrid">
          <label class="field"><span>Username</span><input v-model="ns.username" class="in mono" type="text" placeholder="shop-dev" autocomplete="off" spellcheck="false"></label>
          <label class="field"><span>Password</span><div class="row" style="gap: 6px"><input v-model="ns.password" class="in mono" type="text" placeholder="12+ characters, or leave empty for key only" autocomplete="off"><button type="button" class="btn btn-s" title="Generate" @click="genPw"><AppIcon name="refresh" :size="14" /></button></div></label>
        </div>
        <label class="field"><span>SSH public key (optional)</span><textarea v-model="ns.key" class="in mono" rows="2" placeholder="ssh-ed25519 AAAA… you@laptop" style="min-height: 60px; padding: 10px; font-size: 12px" /></label>
        <div class="row" style="gap: 8px">
          <button type="button" class="btn btn-p" :disabled="!!busy || !ns.username.trim() || (!ns.password && !ns.key.trim())" @click="run('sftp', () => api.post('/api/sftp', { username: ns.username.trim(), site_id: siteId, password: ns.password || undefined, ssh_key: ns.key || undefined }), `SFTP account ${ns.username} created — connect with: sftp ${ns.username}@${host}`).then(() => { if (!err) { ns.open = false; ns.username = ''; ns.key = '' } })">{{ busy === 'sftp' ? 'Creating…' : 'Create account' }}</button>
          <button type="button" class="btn btn-s" @click="ns.open = false">Cancel</button>
        </div>
      </div>
      <div style="overflow-x: auto">
        <table>
          <thead><tr><th>Account</th><th>Connect</th><th>Login</th><th>Folder</th><th><span style="position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0)">Actions</span></th></tr></thead>
          <tbody>
            <tr v-for="u in sftp" :key="u.username">
              <td class="mono" style="font-weight: 500">{{ u.username }}</td>
              <td class="mono" style="font-size: 13px">sftp {{ u.username }}@{{ host }}</td>
              <td style="font-size: 13px">{{ u.has_key ? 'password / SSH key' : 'password' }}</td>
              <td><span :class="`pill ${u.mounted ? 'ok' : 'warn'}`">{{ u.mounted ? u.dir : 'not mounted' }}</span></td>
              <td><ConfirmAction label="Delete" confirm-label="Delete account" question="Delete this SFTP account? Files stay." @confirm="run('sftp-del', () => api.del(`/api/sftp/${encodeURIComponent(u.username)}`), `Deleted ${u.username}.`)" /></td>
            </tr>
            <tr v-if="!sftp.length"><td colspan="5" style="color: #A3AEBD">No SFTP accounts for this site.</td></tr>
          </tbody>
        </table>
      </div>
    </section>
  </div>
</template>

<style scoped>
h2 { font-size: 17px; font-weight: 600; margin: 0; }
.split { display: flex; flex-wrap: wrap; gap: 16px; align-items: flex-start; }
.set-card { flex: 1 1 380px; min-width: 0; }
.fgrid { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 12px; }
.field { display: flex; flex-direction: column; gap: 6px; }
.field > span:first-child { font-size: 13px; font-weight: 500; color: var(--text-2, #C5CEDA); }
.in { min-height: 40px; padding: 0 10px; background: var(--sunken, #0F1318); border: 1px solid var(--btn-line, #2A3441); border-radius: 8px; color: var(--text); font-size: 13px; width: 100%; box-sizing: border-box; }
</style>
