<script setup lang="ts">
import QRCode from 'qrcode'

useHead({ title: 'Account & users · NexaCloud' })

interface Me { username: string; role: 'admin' | 'developer' | 'viewer'; projects: string[]; totp_enabled: boolean; via_token: boolean }
interface Token { id: string; name: string; username: string; scope: string; prefix: string; created_at: string; last_used_at: string | null; expires_at: string | null }
interface PanelUser { username: string; role: string; projects: string[]; totp_enabled: boolean; created_at: string; last_login: string | null }

const api = useApi()
const me = useResource<Me>('/api/auth/me')
const tokens = useResource<Token[]>('/api/account/tokens')
const isAdmin = computed(() => me.data.value?.role === 'admin')
const users = ref<PanelUser[]>([])
const projects = useResource<{ id: string }[]>('/api/projects')
const projectOpts = computed(() => (projects.data.value ?? []).map((p) => ({ value: p.id, label: p.id })))

async function loadUsers() {
  if (!isAdmin.value) return
  try { users.value = await api.get<PanelUser[]>('/api/users') } catch { users.value = [] }
}
watch(isAdmin, (v) => { if (v) loadUsers() }, { immediate: true })

function copy(t: string) { navigator.clipboard?.writeText(t).catch(() => {}) }
const roleLabel: Record<string, string> = { admin: 'Admin — everything', developer: 'Developer — own projects', viewer: 'Viewer — read only' }

// ---------------------------------------------------------------- 2FA
const tfa = reactive({ secret: '', uri: '', qr: '', code: '', codes: [] as string[], busy: false, err: '' })
async function startTfa() {
  tfa.busy = true; tfa.err = ''
  try {
    const r = await api.post<{ secret: string; uri: string }>('/api/account/2fa/setup')
    tfa.secret = r.secret; tfa.uri = r.uri
    tfa.qr = await QRCode.toDataURL(r.uri, { margin: 1, width: 200, color: { dark: '#0B0E12', light: '#FFFFFF' } })
  } catch (e) { if (!(e instanceof SudoCancelled)) tfa.err = errorMessage(e) } finally { tfa.busy = false }
}
async function confirmTfa() {
  tfa.busy = true; tfa.err = ''
  try {
    const r = await api.post<{ recovery_codes: string[] }>('/api/account/2fa/enable', { code: tfa.code.replace(/\s/g, '') })
    tfa.codes = r.recovery_codes; tfa.secret = ''; tfa.qr = ''; tfa.code = ''
    me.refresh()
  } catch (e) { tfa.err = errorMessage(e) } finally { tfa.busy = false }
}
async function disableTfa() {
  tfa.busy = true; tfa.err = ''
  try { await api.post('/api/account/2fa/disable'); me.refresh() } catch (e) { if (!(e instanceof SudoCancelled)) tfa.err = errorMessage(e) } finally { tfa.busy = false }
}

// ---------------------------------------------------------------- password
const pw = reactive({ a: '', b: '', msg: '', err: '', busy: false })
async function changePw() {
  pw.err = ''; pw.msg = ''
  if (pw.a.length < 12) { pw.err = 'Use at least 12 characters.'; return }
  if (pw.a !== pw.b) { pw.err = 'The two passwords are different.'; return }
  pw.busy = true
  try { await api.post('/api/account/password', { password: pw.a }); pw.a = ''; pw.b = ''; pw.msg = 'Password changed.' } catch (e) { if (!(e instanceof SudoCancelled)) pw.err = errorMessage(e) } finally { pw.busy = false }
}

// ---------------------------------------------------------------- tokens
const scopeOpts = [
  { value: 'deploy', label: 'Deploy', hint: 'start deploys, read projects (for CI)' },
  { value: 'read', label: 'Read only', hint: 'GET requests only' },
  { value: 'full', label: 'Full access', hint: 'everything your account can do' },
]
const expiryOpts = [
  { value: 30, label: '30 days' }, { value: 90, label: '90 days' }, { value: 365, label: '1 year' }, { value: 0, label: 'Never expires' },
]
const nt = reactive({ name: '', scope: 'deploy' as string | number | null, expires: 90 as string | number | null, secret: '', err: '', busy: false })
async function createToken() {
  nt.err = ''; nt.busy = true
  try {
    const exp = Number(nt.expires) || undefined
    const r = await api.post<{ secret: string }>('/api/account/tokens', { name: nt.name.trim(), scope: nt.scope, expires_days: exp })
    nt.secret = r.secret; nt.name = ''
    tokens.refresh()
  } catch (e) { if (!(e instanceof SudoCancelled)) nt.err = errorMessage(e) } finally { nt.busy = false }
}
async function revoke(t: Token) {
  try { await api.del(`/api/account/tokens/${encodeURIComponent(t.id)}`); tokens.refresh() } catch (e) { if (!(e instanceof SudoCancelled)) nt.err = errorMessage(e) }
}

// ---------------------------------------------------------------- users
const roleOpts = [
  { value: 'developer', label: 'Developer', hint: 'deploy and change chosen projects' },
  { value: 'viewer', label: 'Viewer', hint: 'read only' },
  { value: 'admin', label: 'Admin', hint: 'full control' },
]
const nu = reactive({ username: '', role: 'developer' as string | number | null, projects: [] as string[], pick: null as string | number | null, password: '', err: '', busy: false, open: false })
watch(() => nu.pick, (v) => { if (v && !nu.projects.includes(String(v))) nu.projects.push(String(v)); nu.pick = null })
function genPassword() {
  const a = new Uint8Array(18); crypto.getRandomValues(a)
  nu.password = btoa(String.fromCharCode(...a)).replace(/[+/=]/g, '').slice(0, 20)
}
async function createUser() {
  nu.err = ''; nu.busy = true
  try {
    await api.post('/api/users', { username: nu.username.trim(), role: nu.role, projects: nu.role === 'developer' ? nu.projects : [], password: nu.password })
    nu.username = ''; nu.projects = []; nu.password = ''; nu.open = false
    loadUsers()
  } catch (e) { if (!(e instanceof SudoCancelled)) nu.err = errorMessage(e) } finally { nu.busy = false }
}
const editing = ref<string | null>(null)
const eu = reactive({ role: '' as string | number | null, projects: [] as string[], pick: null as string | number | null, password: '', err: '', busy: false })
watch(() => eu.pick, (v) => { if (v && !eu.projects.includes(String(v))) eu.projects.push(String(v)); eu.pick = null })
function edit(u: PanelUser) {
  editing.value = editing.value === u.username ? null : u.username
  eu.role = u.role; eu.projects = [...u.projects]; eu.password = ''; eu.err = ''
}
async function saveUser(u: PanelUser, extra: Record<string, unknown> = {}) {
  eu.busy = true; eu.err = ''
  try {
    await api.put(`/api/users/${encodeURIComponent(u.username)}`, { role: eu.role, projects: eu.projects, ...(eu.password ? { password: eu.password } : {}), ...extra })
    editing.value = null; loadUsers()
  } catch (e) { if (!(e instanceof SudoCancelled)) eu.err = errorMessage(e) } finally { eu.busy = false }
}
async function deleteUser(u: PanelUser) {
  eu.busy = true; eu.err = ''
  try { await api.del(`/api/users/${encodeURIComponent(u.username)}`); editing.value = null; loadUsers() } catch (e) { if (!(e instanceof SudoCancelled)) eu.err = errorMessage(e) } finally { eu.busy = false }
}
</script>

<template>
  <header style="display: flex; flex-wrap: wrap; align-items: flex-end; justify-content: space-between; gap: 16px">
    <div style="display: flex; flex-direction: column; gap: 6px">
      <p class="lbl">Settings / Account</p>
      <h1 style="font-size: 30px; font-weight: 600">Account &amp; users</h1>
    </div>
    <div v-if="me.data.value" style="display: flex; align-items: center; gap: 10px; padding: 8px 14px; background: #12171D; border: 1px solid #222A34; border-radius: 10px">
      <AppIcon name="user" :size="18" />
      <span class="mono" style="font-size: 14px; font-weight: 600">{{ me.data.value.username }}</span>
      <span class="pill">{{ me.data.value.role }}</span>
      <span :class="`pill ${me.data.value.totp_enabled ? 'ok' : 'warn'}`">{{ me.data.value.totp_enabled ? '2FA on' : '2FA off' }}</span>
    </div>
  </header>

  <div style="display: flex; flex-wrap: wrap; gap: 16px; align-items: flex-start">
    <!-- 2FA -->
    <section class="card" aria-labelledby="tfa-h" style="flex: 1 1 380px; min-width: 0; padding: 20px; display: flex; flex-direction: column; gap: 14px">
      <div style="display: flex; align-items: center; gap: 10px">
        <AppIcon name="shield" :size="20" />
        <h2 id="tfa-h" style="font-size: 17px; font-weight: 600">Two-factor sign-in</h2>
      </div>
      <p style="font-size: 13px; color: #A3AEBD">After your password, sign-in asks for a 6-digit code from an authenticator app (Google Authenticator, 1Password, Authy, Bitwarden…).</p>

      <template v-if="tfa.codes.length">
        <span class="pill ok" style="align-self: flex-start">Two-factor sign-in is on</span>
        <p style="font-size: 13px; color: #C5CEDA">Save these recovery codes somewhere safe. Each one works once if you lose your phone. They are not shown again.</p>
        <div class="mono" style="display: grid; grid-template-columns: repeat(2, 1fr); gap: 6px; background: #07090C; border: 1px solid #1B222B; border-radius: 10px; padding: 12px 14px; font-size: 13px">
          <span v-for="c in tfa.codes" :key="c">{{ c }}</span>
        </div>
        <div style="display: flex; gap: 8px"><button type="button" class="btn btn-s" @click="copy(tfa.codes.join('\n'))"><AppIcon name="copy" :size="14" /> Copy codes</button><button type="button" class="btn btn-s" @click="tfa.codes = []">I saved them</button></div>
      </template>
      <template v-else-if="me.data.value?.totp_enabled">
        <span class="pill ok" style="align-self: flex-start">On</span>
        <button type="button" class="btn btn-s" style="align-self: flex-start" :disabled="tfa.busy" @click="disableTfa">Turn off two-factor sign-in</button>
      </template>
      <template v-else-if="tfa.secret">
        <div style="display: flex; flex-wrap: wrap; gap: 16px; align-items: center">
          <img v-if="tfa.qr" :src="tfa.qr" alt="QR code for your authenticator app" width="168" height="168" style="border-radius: 10px; background: #fff; padding: 6px">
          <div style="flex: 1 1 180px; display: flex; flex-direction: column; gap: 8px; min-width: 0">
            <span style="font-size: 13px; color: #C5CEDA">1. Scan the code with your app — or type this key:</span>
            <code class="mono" style="font-size: 13px; word-break: break-all; color: #E7ECF2">{{ tfa.secret.replace(/(.{4})/g, '$1 ').trim() }}</code>
            <span style="font-size: 13px; color: #C5CEDA">2. Enter the 6-digit code it shows:</span>
            <div style="display: flex; gap: 8px">
              <input v-model="tfa.code" class="in mono" type="text" inputmode="numeric" autocomplete="one-time-code" placeholder="123456" maxlength="7" aria-label="Code from your app" style="width: 130px; letter-spacing: 2px" @keydown.enter="confirmTfa">
              <button type="button" class="btn btn-p" :disabled="tfa.busy || tfa.code.replace(/\s/g, '').length !== 6" @click="confirmTfa">Turn on</button>
            </div>
          </div>
        </div>
        <button type="button" class="btn btn-s" style="align-self: flex-start" @click="tfa.secret = ''; tfa.qr = ''">Cancel</button>
      </template>
      <template v-else>
        <span class="pill warn" style="align-self: flex-start">Off</span>
        <button type="button" class="btn btn-p" style="align-self: flex-start" :disabled="tfa.busy || me.data.value?.via_token" @click="startTfa"><AppIcon name="lock" :size="15" /> Set up two-factor sign-in</button>
      </template>
      <p v-if="tfa.err" class="small" style="color: var(--bad)" role="alert">{{ tfa.err }}</p>
    </section>

    <!-- Password -->
    <section class="card" aria-labelledby="pw-h" style="flex: 1 1 320px; min-width: 0; padding: 20px; display: flex; flex-direction: column; gap: 14px">
      <div style="display: flex; align-items: center; gap: 10px">
        <AppIcon name="key" :size="20" />
        <h2 id="pw-h" style="font-size: 17px; font-weight: 600">Password</h2>
      </div>
      <template v-if="me.data.value?.username === 'admin'">
        <p style="font-size: 13px; color: #A3AEBD">The built-in admin password is kept in the server config. Change it on the server:</p>
        <code class="mono" style="font-size: 13px; background: #07090C; border: 1px solid #1B222B; border-radius: 8px; padding: 10px 12px">sudo nexacloud reset-password</code>
      </template>
      <template v-else>
        <label style="display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">New password</span><input v-model="pw.a" class="in" type="password" autocomplete="new-password" placeholder="At least 12 characters"></label>
        <label style="display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Repeat</span><input v-model="pw.b" class="in" type="password" autocomplete="new-password" placeholder="Same password again"></label>
        <p v-if="pw.err" class="small" style="color: var(--bad)" role="alert">{{ pw.err }}</p>
        <p v-if="pw.msg" class="small" style="color: var(--ok)">{{ pw.msg }}</p>
        <button type="button" class="btn btn-p" style="align-self: flex-start" :disabled="pw.busy || !pw.a" @click="changePw">Change password</button>
      </template>
    </section>
  </div>

  <!-- API tokens -->
  <section class="card" aria-labelledby="tok-h" style="overflow: hidden">
    <div style="display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px; padding: 16px 20px">
      <div style="display: flex; align-items: center; gap: 10px"><AppIcon name="key" :size="18" /><h2 id="tok-h" style="font-size: 17px; font-weight: 600">API tokens · {{ tokens.data.value?.length ?? 0 }}</h2></div>
      <span style="font-size: 12px; color: #A3AEBD">Send as <code class="mono">Authorization: Bearer nc_…</code></span>
    </div>
    <div style="overflow-x: auto">
      <table>
        <thead><tr><th>Name</th><th>Token</th><th>Scope</th><th v-if="isAdmin">User</th><th>Last used</th><th>Expires</th><th><span style="position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0)">Actions</span></th></tr></thead>
        <tbody>
          <tr v-for="t in tokens.data.value ?? []" :key="t.id">
            <td style="font-weight: 500">{{ t.name }}</td>
            <td class="mono" style="font-size: 13px">{{ t.prefix }}…</td>
            <td><span :class="`pill ${t.scope === 'full' ? 'warn' : ''}`">{{ t.scope }}</span></td>
            <td v-if="isAdmin" class="mono" style="font-size: 13px">{{ t.username }}</td>
            <td style="font-size: 13px; color: #C5CEDA">{{ t.last_used_at ? formatRelative(t.last_used_at) : 'never' }}</td>
            <td style="font-size: 13px; color: #C5CEDA">{{ t.expires_at ? formatDate(t.expires_at) : 'never' }}</td>
            <td><button type="button" class="btn btn-s" :aria-label="`Revoke ${t.name}`" @click="revoke(t)"><AppIcon name="trash" :size="14" /> Revoke</button></td>
          </tr>
          <tr v-if="!(tokens.data.value ?? []).length"><td :colspan="isAdmin ? 7 : 6" style="color: #A3AEBD">No API tokens yet. Create one for CI (GitHub Actions) or scripts.</td></tr>
        </tbody>
      </table>
    </div>
    <div style="border-top: 1px solid #1B222B; padding: 16px 20px; display: flex; flex-direction: column; gap: 12px">
      <template v-if="nt.secret">
        <span class="pill ok" style="align-self: flex-start">Token created — copy it now, it is not shown again</span>
        <div style="display: flex; gap: 8px"><input class="in mono" type="text" :value="nt.secret" readonly aria-label="New token" style="flex: 1"><button type="button" class="btn btn-s" @click="copy(nt.secret)"><AppIcon name="copy" :size="14" /> Copy</button><button type="button" class="btn btn-s" @click="nt.secret = ''">Done</button></div>
      </template>
      <div v-else style="display: flex; flex-wrap: wrap; gap: 10px; align-items: flex-end">
        <label style="flex: 2 1 200px; display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Name</span><input v-model="nt.name" class="in" type="text" placeholder="GitHub Actions — shop" maxlength="80"></label>
        <label style="flex: 1 1 180px; display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Access</span><SearchSelect v-model="nt.scope" :options="me.data.value?.role === 'viewer' ? scopeOpts.filter((o) => o.value === 'read') : scopeOpts" placeholder="Choose access" aria-label="Token access" /></label>
        <label style="flex: 1 1 140px; display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Expires</span><SearchSelect v-model="nt.expires" :options="expiryOpts" placeholder="Choose expiry" aria-label="Token expiry" /></label>
        <button type="button" class="btn btn-p" :disabled="nt.busy || !nt.name.trim() || !nt.scope || me.data.value?.via_token" @click="createToken"><AppIcon name="plus" :size="15" /> Create token</button>
      </div>
      <p v-if="nt.err" class="small" style="color: var(--bad)" role="alert">{{ nt.err }}</p>
    </div>
  </section>

  <!-- Users (admin) -->
  <section v-if="isAdmin" class="card" aria-labelledby="usr-h" style="overflow: hidden">
    <div style="display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px; padding: 16px 20px">
      <div style="display: flex; align-items: center; gap: 10px"><AppIcon name="users" :size="18" /><h2 id="usr-h" style="font-size: 17px; font-weight: 600">Panel users · {{ users.length }}</h2></div>
      <button type="button" class="btn btn-p" @click="nu.open = !nu.open; if (nu.open && !nu.password) genPassword()"><AppIcon name="plus" :size="15" /> Add user</button>
    </div>
    <div v-if="nu.open" style="border-top: 1px solid #1B222B; padding: 16px 20px; display: flex; flex-direction: column; gap: 12px; background: #0F1318">
      <div style="display: flex; flex-wrap: wrap; gap: 10px">
        <label style="flex: 1 1 180px; display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Username</span><input v-model="nu.username" class="in mono" type="text" placeholder="sara" autocomplete="off" spellcheck="false"></label>
        <label style="flex: 1 1 200px; display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Role</span><SearchSelect v-model="nu.role" :options="roleOpts" placeholder="Choose a role" aria-label="Role" /></label>
        <label style="flex: 1 1 220px; display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Password</span><div style="display: flex; gap: 6px"><input v-model="nu.password" class="in mono" type="text" placeholder="At least 12 characters" autocomplete="off" style="flex: 1"><button type="button" class="btn btn-s" title="Generate" @click="genPassword"><AppIcon name="refresh" :size="14" /></button></div></label>
      </div>
      <div v-if="nu.role === 'developer'" style="display: flex; flex-direction: column; gap: 6px">
        <span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Projects they can change</span>
        <div style="display: flex; flex-wrap: wrap; gap: 6px; align-items: center">
          <button v-for="p in nu.projects" :key="p" type="button" class="pill" :aria-label="`Remove ${p}`" style="cursor: pointer; border: 0" @click="nu.projects = nu.projects.filter((x) => x !== p)">{{ p }} ✕</button>
          <div style="width: 240px"><SearchSelect v-model="nu.pick" :options="projectOpts.filter((o) => !nu.projects.includes(o.value))" placeholder="Add a project…" aria-label="Add project" /></div>
        </div>
      </div>
      <p v-if="nu.err" class="small" style="color: var(--bad)" role="alert">{{ nu.err }}</p>
      <div style="display: flex; gap: 8px"><button type="button" class="btn btn-p" :disabled="nu.busy || !nu.username.trim() || nu.password.length < 12" @click="createUser">Create user</button><button type="button" class="btn btn-s" @click="nu.open = false">Cancel</button></div>
    </div>
    <div style="overflow-x: auto">
      <table>
        <thead><tr><th>User</th><th>Role</th><th>Projects</th><th>2FA</th><th>Last sign-in</th><th><span style="position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0)">Actions</span></th></tr></thead>
        <tbody>
          <template v-for="u in users" :key="u.username">
            <tr>
              <td class="mono" style="font-weight: 500">{{ u.username }}</td>
              <td style="font-size: 13px; color: #C5CEDA">{{ roleLabel[u.role] ?? u.role }}</td>
              <td style="font-size: 13px; color: #C5CEDA">{{ u.role === 'developer' ? (u.projects.join(', ') || 'none yet') : 'all' }}</td>
              <td><span :class="`pill ${u.totp_enabled ? 'ok' : 'warn'}`">{{ u.totp_enabled ? 'On' : 'Off' }}</span></td>
              <td style="font-size: 13px; color: #C5CEDA">{{ u.last_login ? formatRelative(u.last_login) : 'never' }}</td>
              <td><button v-if="u.username !== 'admin'" type="button" class="btn btn-s" :aria-expanded="editing === u.username" @click="edit(u)"><AppIcon name="edit" :size="14" /> Edit</button></td>
            </tr>
            <tr v-if="editing === u.username">
              <td colspan="6" style="background: #0F1318; white-space: normal">
                <div style="display: flex; flex-direction: column; gap: 12px">
                  <div style="display: flex; flex-wrap: wrap; gap: 10px">
                    <label style="flex: 1 1 200px; display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">Role</span><SearchSelect v-model="eu.role" :options="roleOpts" placeholder="Choose a role" aria-label="Role" /></label>
                    <label style="flex: 1 1 220px; display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">New password</span><input v-model="eu.password" class="in mono" type="text" placeholder="Leave empty to keep" autocomplete="off"></label>
                  </div>
                  <div v-if="eu.role === 'developer'" style="display: flex; flex-wrap: wrap; gap: 6px; align-items: center">
                    <button v-for="p in eu.projects" :key="p" type="button" class="pill" :aria-label="`Remove ${p}`" style="cursor: pointer; border: 0" @click="eu.projects = eu.projects.filter((x) => x !== p)">{{ p }} ✕</button>
                    <div style="width: 240px"><SearchSelect v-model="eu.pick" :options="projectOpts.filter((o) => !eu.projects.includes(o.value))" placeholder="Add a project…" aria-label="Add project" /></div>
                  </div>
                  <p v-if="eu.err" class="small" style="color: var(--bad)" role="alert">{{ eu.err }}</p>
                  <div style="display: flex; flex-wrap: wrap; gap: 8px">
                    <button type="button" class="btn btn-p" :disabled="eu.busy" @click="saveUser(u)">Save</button>
                    <button v-if="u.totp_enabled" type="button" class="btn btn-s" :disabled="eu.busy" @click="saveUser(u, { reset_2fa: true })">Reset 2FA</button>
                    <ConfirmAction :label="`Delete ${u.username}`" confirm-label="Delete user" :busy="eu.busy" @confirm="deleteUser(u)" />
                  </div>
                </div>
              </td>
            </tr>
          </template>
        </tbody>
      </table>
    </div>
  </section>
</template>
