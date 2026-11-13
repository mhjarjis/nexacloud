<script setup lang="ts">
// Board 09 · Install & Import — same markup as the design, real data.
import type { DatabasesInfo, SecurityInfo, Site, SystemInfo } from '~/types/api'

useHead({ title: 'Install & Import · NexaCloud' })
const api = useApi()
const sys = useResource<SystemInfo>('/api/system')
const health = useResource<{ version: string }>('/api/health')
const sites = useResource<Site[]>('/api/sites')
const projects = useResource<{ id: string; repo: string; branch: string }[]>('/api/projects')
const dbs = useResource<DatabasesInfo>('/api/databases')
const sec = useResource<SecurityInfo>('/api/security')
const host = computed(() => sys.data.value?.hostname ?? 'server')

const step = ref(1)
function go(n: number) { step.value = Math.max(1, Math.min(4, n)) }
const steps = computed(() => [['Install agent', 'One command'], ['Secure panel', 'Domain, firewall'], ['Import sites', 'Scan what exists'], ['Connect GitHub', 'Auto-deploys']].map(([title, sub], i) => {
  const n = i + 1, done = n < step.value || (n === 1), cur = n === step.value
  return { n, title, sub, mark: done && !cur ? '✓' : String(n), current: (cur ? 'step' : undefined) as 'step' | undefined, bg: cur ? '#13211B' : '#12171D', border: cur ? '#3DDC97' : '#222A34', dotBg: done || cur ? '#3DDC97' : '#1B222B', dotFg: done || cur ? '#06130D' : '#A3AEBD' }
}))

// ---- step 1
const INSTALL = 'curl -fsSL https://raw.githubusercontent.com/mhjarjis/nexacloud/main/scripts/install.sh | sudo bash'
const copied = ref(false)
async function copyCmd() { await navigator.clipboard?.writeText(INSTALL); copied.value = true }
const installLog = computed(() => `✓ ${sys.data.value?.os ?? 'Ubuntu'} detected · ${sys.data.value?.kernel ?? ''}\n✓ nexacloud ${health.data.value?.version ?? ''} installed (checksum verified)\n✓ Config in /etc/nexacloud, data in /var/lib/nexacloud\n✓ nexacloud.service is running\n→ You're using it now: ${typeof window !== 'undefined' ? window.location.origin : ''}`)
const root = computed(() => sys.data.value?.disks?.find((d) => d.mount === '/') ?? sys.data.value?.disks?.[0])
const checks = computed(() => {
  const s = sys.data.value
  const nginx = (sites.data.value ?? []).length
  return [
    { k: 'Operating system', v: s ? s.os.replace(/ LTS$/, '') : '…', cls: s?.os.includes('Ubuntu') ? 'ok' : 'warn' },
    { k: 'CPU / memory', v: s ? `${s.cpu_count} vCPU · ${Math.round(s.memory.total / 1024 ** 3)} GB` : '…', cls: 'ok' },
    { k: 'Free disk', v: root.value ? `${Math.round((root.value.total - root.value.used) / 1024 ** 3)} GB` : '…', cls: root.value && root.value.total - root.value.used < 5 * 1024 ** 3 ? 'warn' : 'ok' },
    { k: 'Existing Nginx sites', v: nginx ? `${nginx} found · can adopt` : 'None', cls: nginx ? 'info' : 'ok' },
    { k: 'Firewall', v: sec.data.value?.ufw.active ? 'On' : 'Off', cls: sec.data.value?.ufw.active ? 'ok' : 'warn' },
    { k: 'Agent', v: health.data.value ? `nexacloud ${health.data.value.version}` : '…', cls: 'ok' },
  ]
})

// ---- step 3
const found = computed(() => {
  const out: { id: string; name: string; kind: string; detail: string; managed: boolean; isSite: boolean }[] = []
  for (const s of sites.data.value ?? []) {
    if (s.id === '_' || s.id === 'localhost') continue
    const managed = s.config_path.includes('nexacloud-')
    out.push({ id: s.id, name: s.id, kind: ({ laravel: 'Laravel', php: 'Plain PHP', static: 'Static', proxy: 'Proxy' } as Record<string, string>)[s.kind] ?? s.kind, detail: `${s.root ?? s.proxy_pass ?? ''}${s.php_version ? ' · PHP ' + s.php_version : ''}${managed ? ' · already managed' : ''}`, managed, isSite: true })
  }
  const d = dbs.data.value
  if (d?.mysql.available) out.push({ id: 'db:mysql', name: 'MySQL databases', kind: `${d.mysql.databases.length} found`, detail: d.mysql.databases.map((x) => x.name).join(', ') || '—', managed: true, isSite: false })
  if (d?.postgres.available) out.push({ id: 'db:pg', name: 'PostgreSQL databases', kind: `${d.postgres.databases.length} found`, detail: d.postgres.databases.map((x) => x.name).join(', ') || '—', managed: true, isSite: false })
  return out
})
const pick = reactive<Record<string, boolean>>({})
watch(found, (f) => { for (const x of f) if (pick[x.id] === undefined) pick[x.id] = x.isSite && !x.managed }, { immediate: true })
const toImport = computed(() => found.value.filter((x) => x.isSite && !x.managed && pick[x.id]))
const importLog = ref<string[]>([])
const importing = ref(false)
async function planApply(request: { kind: string; params: Record<string, unknown> }) {
  const pv = await api.post<{ plan_hash: string }>('/api/plans/preview', request)
  const st = await api.post<{ change_id: string }>('/api/plans/apply', { ...request, plan_hash: pv.plan_hash })
  for (let i = 0; i < 120; i++) {
    await new Promise((r) => setTimeout(r, 1000))
    const c = await api.get<{ status: string }>(`/api/changes/${st.change_id}`)
    if (['applied', 'reverted', 'failed', 'revert_failed'].includes(c.status)) return c.status
  }
  return 'timeout'
}
async function runImport() {
  importing.value = true
  importLog.value = []
  for (const x of toImport.value) {
    importLog.value.push(`… ${x.name}`)
    try {
      const st = await planApply({ kind: 'site_adopt', params: { id: x.id, accept_custom: true } })
      if (st === 'applied') {
        try { await api.post('/api/projects', { site_id: x.id }) } catch { /* proxy sites or already a project */ }
        importLog.value[importLog.value.length - 1] = `✓ ${x.name} — managed, original config kept disabled`
      } else {
        importLog.value[importLog.value.length - 1] = `✕ ${x.name} — ${st}, nothing changed (see Activity)`
      }
    } catch (e) {
      if (e instanceof SudoCancelled) { importLog.value.pop(); break }
      importLog.value[importLog.value.length - 1] = `✕ ${x.name} — ${errorMessage(e)}`
    }
  }
  importing.value = false
  sites.refresh(); projects.refresh()
}

// ---- step 4
const repoFor = reactive<Record<string, { repo: string; branch: string }>>({})
const maps = computed(() => (projects.data.value ?? []).map((p) => ({ id: p.id, repo: p.repo, branch: p.branch, state: p.repo ? 'Connected' : 'Not connected', cls: p.repo ? 'ok' : 'off' })))
watch(maps, (m) => { for (const x of m) if (!repoFor[x.id]) repoFor[x.id] = { repo: '', branch: 'main' } }, { immediate: true })
const connectMsg = reactive<Record<string, string>>({})
async function connect(id: string) {
  const f = repoFor[id]!
  try { await api.put(`/api/projects/${encodeURIComponent(id)}`, { repo: f.repo.trim(), branch: f.branch.trim() || 'main' }); connectMsg[id] = 'Connected — open the project to copy its deploy key.'; projects.refresh() } catch (e) { if (!(e instanceof SudoCancelled)) connectMsg[id] = errorMessage(e) }
}
const origin = computed(() => (typeof window !== 'undefined' ? window.location.host : 'panel'))
</script>

<template>
  <header style="display: flex; flex-direction: column; gap: 6px">
    <p class="lbl">{{ host }} / Install &amp; Import</p>
    <h1 style="font-size: 30px; font-weight: 600">Set up NexaCloud in 4 steps</h1>
    <p style="font-size: 14px; color: #A3AEBD">About 3 minutes. Your running sites keep working the whole time.</p>
  </header>

  <ol aria-label="Setup steps" style="list-style: none; margin: 0; padding: 0; display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 10px">
    <li v-for="s in steps" :key="s.n">
      <button type="button" :aria-current="s.current" :style="`width: 100%; min-height: 64px; display: flex; align-items: center; gap: 12px; padding: 12px 14px; border-radius: 10px; cursor: pointer; text-align: left; background: ${s.bg}; border: 1px solid ${s.border}; color: #E7ECF2`" @click="go(s.n)">
        <span :style="`width: 30px; height: 30px; border-radius: 50%; display: grid; place-items: center; font-weight: 700; font-size: 13px; flex: none; background: ${s.dotBg}; color: ${s.dotFg}`">{{ s.mark }}</span>
        <span style="display: flex; flex-direction: column; gap: 2px"><span style="font-size: 14px; font-weight: 600">{{ s.title }}</span><span style="font-size: 12px; color: #A3AEBD">{{ s.sub }}</span></span>
      </button>
    </li>
  </ol>

  <div v-if="step === 1" style="display: flex; flex-wrap: wrap; gap: 16px; align-items: flex-start">
    <section class="card" aria-labelledby="ins" style="flex: 3 1 520px; min-width: 0; padding: 22px; display: flex; flex-direction: column; gap: 16px">
      <h2 id="ins" style="font-size: 19px; font-weight: 600">1. Run one command on your server</h2>
      <p style="font-size: 14px; color: #C5CEDA; line-height: 1.5">Open the Hostinger web console (or SSH in as root) and paste — the same command upgrades an existing install:</p>
      <div style="display: flex; align-items: stretch; gap: 0; border: 1px solid #2A3441; border-radius: 10px; overflow: hidden">
        <span class="mono" style="flex: 1; min-width: 0; padding: 14px 16px; background: #07090C; font-size: 13px; color: #E7ECF2; overflow-x: auto; white-space: nowrap"><span style="color: #5BE6A9">$</span> {{ INSTALL }}</span>
        <button type="button" class="btn btn-p" style="border-radius: 0; min-height: 48px" @click="copyCmd">{{ copied ? 'Copied' : 'Copy' }}</button>
      </div>
      <div class="mono" style="background: #07090C; border: 1px solid #1B222B; border-radius: 10px; padding: 14px 16px; font-size: 12px; line-height: 1.8; color: #C5CEDA; white-space: pre-wrap">{{ installLog }}</div>
      <div style="display: flex; flex-wrap: wrap; gap: 10px; align-items: center">
        <span class="pill ok">Agent connected</span>
        <span style="font-size: 12px; color: #A3AEBD; flex: 1">This server already runs NexaCloud {{ health.data.value?.version ?? '' }}.</span>
        <button type="button" class="btn btn-s" @click="go(2)">Next</button>
      </div>
    </section>
    <section style="flex: 2 1 340px; min-width: 0; display: flex; flex-direction: column; gap: 16px">
      <div class="card" style="padding: 20px; display: flex; flex-direction: column; gap: 10px">
        <h2 style="font-size: 16px; font-weight: 600">Pre-flight check</h2>
        <div v-for="c in checks" :key="c.k" style="display: flex; align-items: center; gap: 10px; padding: 6px 0; border-bottom: 1px solid #1B222B"><span style="flex: 1; font-size: 13px">{{ c.k }}</span><span :class="`pill ${c.cls}`">{{ c.v }}</span></div>
      </div>
      <div class="card" style="padding: 20px; display: flex; flex-direction: column; gap: 8px">
        <h2 style="font-size: 16px; font-weight: 600">What gets installed</h2>
        <p style="font-size: 13px; color: #C5CEDA; line-height: 1.55">One Rust binary (<span class="mono">nexacloud</span>, ≈ 15 MB) running as a systemd service, plus the Nuxt panel it serves. Nothing else on your server changes until you click something.</p>
        <p class="mono" style="font-size: 12px; color: #A3AEBD; line-height: 1.7">update:    re-run the install command<br>password:  sudo nexacloud reset-password<br>uninstall: … | sudo bash -s -- --uninstall</p>
      </div>
    </section>
  </div>

  <section v-if="step === 2" class="card" aria-labelledby="sec" style="padding: 22px; display: flex; flex-direction: column; gap: 16px; max-width: 860px">
    <h2 id="sec" style="font-size: 19px; font-weight: 600">2. Secure the panel</h2>
    <PanelDomain />
    <FirewallCard :active="!!sec.data.value?.ufw.active" @changed="sec.refresh()" />
    <label style="display: flex; align-items: center; gap: 10px; font-size: 14px; color: #6B7684"><input type="checkbox" disabled style="width: 18px; height: 18px">Require two-factor authentication (TOTP) · coming soon</label>
    <label style="display: flex; align-items: center; gap: 10px; font-size: 14px; color: #6B7684"><input type="checkbox" disabled style="width: 18px; height: 18px">Only allow my current IP address · coming soon</label>
    <div style="display: flex; gap: 10px"><button type="button" class="btn" @click="go(1)">Back</button><button type="button" class="btn btn-p" @click="go(3)">Next: scan server</button></div>
  </section>

  <section v-if="step === 3" class="card" aria-labelledby="scan" style="padding: 22px; display: flex; flex-direction: column; gap: 16px">
    <div style="display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px">
      <div style="display: flex; flex-direction: column; gap: 4px"><h2 id="scan" style="font-size: 19px; font-weight: 600">3. We found what's already running</h2><p style="font-size: 13px; color: #A3AEBD">Read-only scan. Pick what NexaCloud should manage; files stay where they are and each original nginx file is kept (disabled) so it can be undone.</p></div>
      <span :class="`pill ${sites.loading.value ? 'info' : 'ok'}`">{{ sites.loading.value ? 'Scanning…' : `Scan finished · ${found.length} items` }}</span>
    </div>
    <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr)); gap: 10px">
      <label v-for="f in found" :key="f.id" :style="`display: flex; align-items: flex-start; gap: 12px; padding: 12px 14px; background: #0F1318; border: 1px solid #222A34; border-radius: 10px; cursor: ${f.managed ? 'default' : 'pointer'}`">
        <input v-model="pick[f.id]" type="checkbox" :disabled="f.managed" :checked="f.managed || pick[f.id]" style="width: 18px; height: 18px; accent-color: #3DDC97; margin-top: 2px">
        <span style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 4px">
          <span style="display: flex; flex-wrap: wrap; align-items: center; gap: 8px"><span style="font-size: 14px; font-weight: 600">{{ f.name }}</span><span class="tag">{{ f.kind }}</span></span>
          <span class="mono" style="font-size: 12px; color: #A3AEBD; overflow: hidden; text-overflow: ellipsis; white-space: nowrap">{{ f.detail }}</span>
        </span>
      </label>
    </div>
    <div v-if="importLog.length" class="mono" style="background: #07090C; border: 1px solid #1B222B; border-radius: 10px; padding: 14px 16px; font-size: 12px; line-height: 1.8; color: #C5CEDA; white-space: pre-wrap">{{ importLog.join('\n') }}</div>
    <div style="display: flex; flex-wrap: wrap; gap: 10px; align-items: center"><button type="button" class="btn" @click="go(2)">Back</button><button type="button" class="btn btn-p" :disabled="importing || !toImport.length" @click="runImport">{{ importing ? 'Importing…' : `Import ${toImport.length} ${toImport.length === 1 ? 'item' : 'items'}` }}</button><button type="button" class="btn" @click="go(4)">Next</button><span style="font-size: 12px; color: #A3AEBD">You can undo an import at any time from Activity.</span></div>
  </section>

  <section v-if="step === 4" class="card" aria-labelledby="gh" style="padding: 22px; display: flex; flex-direction: column; gap: 16px">
    <h2 id="gh" style="font-size: 19px; font-weight: 600">4. Connect GitHub</h2>
    <p style="font-size: 14px; color: #C5CEDA">Match each project to its repository and branch. NexaCloud creates a read-only deploy key per repository (no GitHub account access needed); add it under the repo's Settings → Deploy keys. Deploys then run on every push.</p>
    <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); gap: 10px">
      <div v-for="m in maps" :key="m.id" style="display: flex; flex-direction: column; gap: 8px; padding: 10px 12px; background: #0F1318; border: 1px solid #222A34; border-radius: 10px">
        <div style="display: flex; align-items: center; gap: 10px">
          <span style="flex: 1; font-size: 14px; font-weight: 600">{{ m.id }}</span>
          <span v-if="m.repo" class="mono" style="font-size: 12px; color: #C5CEDA">{{ m.repo.replace(/^.*[:/]([^/:]+\/[^/]+?)(\.git)?$/, '$1') }} @ {{ m.branch }}</span>
          <span :class="`pill ${m.cls}`">{{ m.state }}</span>
        </div>
        <div v-if="!m.repo && repoFor[m.id]" style="display: flex; gap: 6px">
          <input v-model="repoFor[m.id]!.repo" class="in mono" type="text" placeholder="git@github.com:owner/repo.git" aria-label="Repository" style="flex: 1; min-height: 36px">
          <input v-model="repoFor[m.id]!.branch" class="in mono" type="text" placeholder="main" aria-label="Branch" style="width: 90px; min-height: 36px">
          <button type="button" class="btn btn-s" :disabled="!repoFor[m.id]!.repo.trim()" @click="connect(m.id)">Connect</button>
        </div>
        <span v-if="connectMsg[m.id]" style="font-size: 12px; color: #A3AEBD">{{ connectMsg[m.id] }}</span>
      </div>
      <p v-if="!maps.length" style="font-size: 13px; color: #A3AEBD">No projects yet — import sites in step 3 or create a new project.</p>
    </div>
    <div style="display: flex; gap: 10px"><button type="button" class="btn" @click="go(3)">Back</button><NuxtLink class="btn btn-p" to="/">Finish · open dashboard</NuxtLink></div>
  </section>

  <section class="card" aria-labelledby="arch" style="padding: 22px; display: flex; flex-direction: column; gap: 16px">
    <h2 id="arch" style="font-size: 17px; font-weight: 600">How it fits together</h2>
    <div style="display: flex; flex-wrap: wrap; align-items: stretch; gap: 10px">
      <div style="flex: 1 1 200px; padding: 14px; border-radius: 10px; background: #0F1318; border: 1px solid #222A34; display: flex; flex-direction: column; gap: 6px"><span class="lbl">Your browser</span><span style="font-size: 15px; font-weight: 600">Nuxt panel</span><span style="font-size: 12px; color: #A3AEBD">{{ origin }} · TLS · sudo mode</span></div>
      <div style="display: grid; place-items: center; color: #A3AEBD; font-size: 12px; padding: 0 4px" aria-hidden="true">→ HTTPS + live logs →</div>
      <div style="flex: 1 1 220px; padding: 14px; border-radius: 10px; background: #13211B; border: 1px solid #2B5A44; display: flex; flex-direction: column; gap: 6px"><span class="lbl" style="color: #A9E9CB">On the server</span><span style="font-size: 15px; font-weight: 600">nexacloud agent (Rust)</span><span style="font-size: 12px; color: #C5CEDA">REST API, job queue, monitoring, SQLite state</span></div>
      <div style="display: grid; place-items: center; color: #A3AEBD; font-size: 12px; padding: 0 4px" aria-hidden="true">→ manages →</div>
      <div style="flex: 2 1 300px; padding: 14px; border-radius: 10px; background: #0F1318; border: 1px solid #222A34; display: flex; flex-wrap: wrap; gap: 6px; align-content: flex-start">
        <span class="tag">nginx</span><span class="tag">php-fpm 8.1–8.5</span><span class="tag">FrankenPHP</span><span class="tag">MySQL</span><span class="tag">PostgreSQL</span><span class="tag">Redis</span><span class="tag">Supervisor</span><span class="tag">cron</span><span class="tag">certbot</span><span class="tag">git</span><span class="tag">UFW</span><span class="tag">rclone backups</span>
      </div>
    </div>
  </section>
</template>

<style scoped>
.in { padding: 0 10px; box-sizing: border-box; }
</style>
