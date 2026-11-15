<script setup lang="ts">
import type { ChangePreviewData, ChangeStarted, ChangeSummary, JobStatus, SecurityInfo, Tone } from '~/types/api'

useHead({ title: 'Security · NexaCloud' })

const api = useApi()
const sec = useResource<SecurityInfo>('/api/security')

// ---- One-click fix: hide the nginx version (change pipeline: preview → apply → result)
type FixStep = 'idle' | 'previewing' | 'preview' | 'applying' | 'running' | 'result'
const fix = reactive({
  step: 'idle' as FixStep,
  preview: null as ChangePreviewData | null,
  started: null as ChangeStarted | null,
  result: null as ChangeSummary | null,
  error: null as string | null,
})

async function previewFix() {
  fix.step = 'previewing'
  fix.error = null
  fix.result = null
  try {
    fix.preview = await api.post<ChangePreviewData>('/api/advisor/nginx-hardening/preview')
    fix.step = 'preview'
  } catch (err) {
    fix.error = errorMessage(err)
    fix.step = 'idle'
  }
}

async function applyFix() {
  if (!fix.preview) return
  fix.step = 'applying'
  fix.error = null
  try {
    fix.started = await api.post<ChangeStarted>('/api/advisor/nginx-hardening/apply', { plan_hash: fix.preview.plan_hash })
    fix.step = 'running'
  } catch (err) {
    fix.step = 'preview'
    if (!(err instanceof SudoCancelled)) fix.error = errorMessage(err)
  }
}

async function onFixFinished(_s: JobStatus) {
  if (!fix.started) return
  try {
    fix.result = await api.get<ChangeSummary>(`/api/changes/${encodeURIComponent(fix.started.change_id)}`)
  } catch (err) {
    fix.error = errorMessage(err)
  }
  fix.step = 'result'
}

function resetFix() {
  fix.step = 'idle'
  fix.preview = null
  fix.started = null
  fix.result = null
  fix.error = null
}

const fixPill = computed<{ tone: Tone; label: string } | null>(() => {
  if (fix.step === 'result' && fix.result) return changeStatus(fix.result.status)
  if (fix.step === 'running' || fix.step === 'applying') return { tone: 'info', label: 'Applying' }
  if (fix.preview?.nothing_to_do) return { tone: 'ok', label: 'Nothing to do' }
  if (fix.preview) return { tone: 'warn', label: 'Version visible' }
  return null
})

interface Check {
  id: string
  title: string
  tone: Tone
  state: string
  why: string
  hint?: string
}

const checks = computed<Check[]>(() => {
  const s = sec.data.value
  if (!s) return []
  const ufw: Check = {
    id: 'ufw',
    title: 'Firewall (UFW)',
    why: 'A firewall closes every port you did not open on purpose, so a forgotten database or debug server is not reachable from the internet.',
    ...(!s.ufw.installed
      ? { tone: 'bad', state: 'Not installed', hint: 'Install it with: sudo apt install ufw' }
      : !s.ufw.active
        ? { tone: 'bad', state: 'Inactive', hint: 'Allow SSH first (sudo ufw allow OpenSSH), then run sudo ufw enable.' }
        : { tone: 'ok', state: `Active · ${s.ufw.rules.length} ${s.ufw.rules.length === 1 ? 'rule' : 'rules'}` }),
  }
  const f2b: Check = {
    id: 'fail2ban',
    title: 'Fail2ban',
    why: 'Bans IP addresses that keep guessing passwords, which stops automated SSH brute-force attacks.',
    ...(!s.fail2ban.installed
      ? { tone: 'warn', state: 'Not installed', hint: 'Install it with: sudo apt install fail2ban' }
      : !s.fail2ban.active
        ? { tone: 'bad', state: 'Stopped', hint: 'Start it with: sudo systemctl enable --now fail2ban' }
        : { tone: 'ok', state: 'Active' }),
  }
  const upgrades: Check = {
    id: 'unattended',
    title: 'Automatic security updates',
    why: 'Installs Ubuntu security patches automatically, so known holes get closed even when nobody logs in.',
    ...(s.unattended_upgrades === true
      ? { tone: 'ok', state: 'On' }
      : s.unattended_upgrades === false
        ? { tone: 'warn', state: 'Off', hint: 'Turn on with: sudo apt install unattended-upgrades && sudo dpkg-reconfigure -plow unattended-upgrades' }
        : { tone: 'off', state: 'Unknown' }),
  }
  const ssh: Check = {
    id: 'ssh',
    title: 'SSH password login',
    why: 'Passwords can be guessed; SSH keys cannot. With password login off, only people holding your key can get a shell.',
    ...(s.ssh_password_auth === true
      ? { tone: 'warn', state: 'Allowed', hint: 'After confirming key login works, set PasswordAuthentication no in /etc/ssh/sshd_config and reload ssh.' }
      : s.ssh_password_auth === false
        ? { tone: 'ok', state: 'Disabled (keys only)' }
        : { tone: 'off', state: 'Unknown', hint: 'NexaCloud could not read the SSH server settings (sshd -T).' }),
  }
  return [ufw, f2b, upgrades, ssh]
})

const okCount = computed(() => checks.value.filter((c) => c.tone === 'ok').length)

// ---- Server-wide IP block list (nginx, all sites)
const blocked = useResource<{ ips: string[] }>('/api/blocklist')
const blockText = ref('')
watch(() => blocked.data.value, (v) => { if (v) blockText.value = v.ips.join('\n') }, { immediate: true })
const blockBusy = ref(false)
const blockMsg = ref('')
const blockErr = ref('')
async function saveBlocklist() {
  blockBusy.value = true; blockMsg.value = ''; blockErr.value = ''
  const ips = blockText.value.split(/[\s,]+/).map((x) => x.trim()).filter(Boolean)
  try {
    const req = { kind: 'ip_block', params: { ips } }
    const pv = await api.post<{ plan_hash: string }>('/api/plans/preview', req)
    const st = await api.post<{ change_id: string }>('/api/plans/apply', { ...req, plan_hash: pv.plan_hash })
    for (let i = 0; i < 60; i++) {
      const c = await api.get<{ status: string; error?: string | null }>(`/api/changes/${st.change_id}`)
      if (['applied', 'reverted', 'failed', 'revert_failed'].includes(c.status)) {
        if (c.status === 'applied') blockMsg.value = ips.length ? `${ips.length} address(es) blocked on every site.` : 'Block list cleared.'
        else blockErr.value = `${c.status}: ${c.error ?? 'see Activity'}`
        break
      }
      await new Promise((r) => setTimeout(r, 1000))
    }
    blocked.refresh()
  } catch (e) { if (!(e instanceof SudoCancelled)) blockErr.value = errorMessage(e) } finally { blockBusy.value = false }
}
const sftpAll = useResource<{ username: string; site_id: string; dir: string; mounted: boolean; has_key: boolean }[]>('/api/sftp')


// ---- Malware scan (ClamAV)
interface MalwareStatus { installed: boolean; signatures_updated: number | null; last: { path: string; finished_at: string; scanned_files: number; found: { file: string; signature: string }[]; ok: boolean } | null; quarantine: string[] }
const mal = useResource<MalwareStatus>('/api/malware')
const malJob = ref<string | null>(null)
const malPath = ref<string | number | null>('/var/www')
const malErr = ref('')
const scanTargets = computed(() => ['/var/www', '/srv', '/home', ...((sftpAll.data.value ?? []).map((u) => u.dir))].filter((v, i, a) => a.indexOf(v) === i))
async function malStart(path: string, body?: Record<string, unknown>) {
  malErr.value = ''
  try { malJob.value = (await api.post<{ job_id: string }>(path, body)).job_id } catch (e) { if (!(e instanceof SudoCancelled)) malErr.value = errorMessage(e) }
}
async function quarantineFile(file: string) {
  malErr.value = ''
  try { await api.post('/api/malware/quarantine', { file }); mal.refresh() } catch (e) { if (!(e instanceof SudoCancelled)) malErr.value = errorMessage(e) }
}
</script>

<template>
  <PageHeader title="Security" crumb="Basic server hardening">
    <StatusPill
      v-if="checks.length"
      :tone="okCount === checks.length ? 'ok' : okCount >= checks.length - 1 ? 'warn' : 'bad'"
      :label="`${okCount} of ${checks.length} checks passing`"
    />
  </PageHeader>

  <InfoNote>One-click fixes go through NexaCloud's change pipeline: you see exactly what changes, files are snapshotted first, and anything that fails is reverted automatically. The other checks tell you how to fix them by hand for now.</InfoNote>

  <FirewallCard v-if="sec.data.value" :active="sec.data.value.ufw.active" @changed="sec.refresh()" />
  <PanelDomain />

  <section class="card fix" aria-labelledby="fix-nginx-h">
    <div class="check" style="border-bottom: 0">
      <div class="check-icon info" aria-hidden="true"><AppIcon name="security" :size="16" :stroke="2.2" /></div>
      <div class="grow stack" style="gap: 10px">
        <div class="row wrap" style="gap: 10px; justify-content: space-between">
          <div class="stack" style="gap: 2px">
            <span class="lbl">One-click fix</span>
            <h2 id="fix-nginx-h" class="check-title">Hide nginx version</h2>
          </div>
          <StatusPill v-if="fixPill" v-bind="fixPill" />
        </div>
        <p class="text-2 check-why">
          nginx prints its version number in error pages and the <span class="mono">Server</span> header, which helps attackers look up known holes.
          This adds <span class="mono">server_tokens off;</span> in NexaCloud's own drop-in file
          (<span class="mono">/etc/nginx/conf.d/zz-nexacloud.conf</span>); your config files are not edited.
        </p>

        <div v-if="fix.step === 'idle' || fix.step === 'previewing'" class="row wrap" style="gap: 10px">
          <button type="button" class="btn btn-s" :disabled="fix.step === 'previewing'" @click="previewFix">
            <span v-if="fix.step === 'previewing'" class="spinner" aria-hidden="true" />
            Preview
          </button>
          <span class="small muted">Shows exactly what would change. Nothing is applied yet.</span>
        </div>

        <ChangePreview
          v-if="(fix.step === 'preview' || fix.step === 'applying') && fix.preview"
          :preview="fix.preview"
          :applying="fix.step === 'applying'"
          apply-label="Apply"
          @apply="applyFix"
          @cancel="resetFix"
        />
        <div v-if="fix.step === 'preview' && fix.preview?.nothing_to_do">
          <button type="button" class="btn btn-s btn-ghost" @click="resetFix">Close</button>
        </div>

        <div v-if="fix.step === 'running' && fix.started" class="stack" style="gap: 10px">
          <div class="progress" role="progressbar" aria-label="Applying change" aria-busy="true"><span /></div>
          <JobLog :job-id="fix.started.job_id" status="running" @finished="onFixFinished" />
        </div>

        <div v-if="fix.step === 'result' && fix.result" class="stack fix-result" style="gap: 8px">
          <div class="progress" :class="fix.result.status === 'applied' ? 'done' : 'failed'"><span /></div>
          <p v-if="fix.result.status === 'applied'" style="color: var(--ok)">
            Done. nginx no longer shows its version.
          </p>
          <p v-else-if="fix.result.status === 'reverted'" style="color: var(--warn)">
            The change did not pass its checks and was reverted automatically: {{ fix.result.error }}
          </p>
          <p v-else style="color: var(--bad)">{{ fix.result.error ?? 'The change did not apply.' }}</p>
          <div class="row wrap" style="gap: 12px">
            <NuxtLink v-if="fix.result.can_rollback" :to="`/activity?open=${fix.result.id}`" class="btn btn-s">
              <AppIcon name="undo" :size="14" :stroke="2" /> Rollback
            </NuxtLink>
            <NuxtLink :to="`/activity?open=${fix.result.id}`" class="small">View in Activity</NuxtLink>
            <button type="button" class="btn btn-s btn-ghost" @click="resetFix">Close</button>
          </div>
        </div>

        <p v-if="fix.error" class="small" style="color: var(--bad)" role="alert">{{ fix.error }}</p>
      </div>
    </div>
  </section>

  <div v-if="sec.loading.value" class="card card-pad"><SkeletonRows :rows="6" label="Loading security checks" /></div>
  <ErrorBox
    v-else-if="sec.error.value && !sec.data.value"
    :message="sec.error.value"
    :retrying="sec.refreshing.value"
    @retry="sec.refresh()"
  />
  <section v-else class="card" aria-label="Security checks">
    <div v-for="c in checks" :key="c.id" class="check">
      <div class="check-icon" :class="c.tone" aria-hidden="true">
        <AppIcon :name="c.tone === 'ok' ? 'check' : c.tone === 'off' ? 'info' : 'alert'" :size="16" :stroke="2.2" />
      </div>
      <div class="grow stack" style="gap: 4px">
        <div class="row wrap" style="gap: 10px; justify-content: space-between">
          <h2 class="check-title">{{ c.title }}</h2>
          <StatusPill :tone="c.tone" :label="c.state" />
        </div>
        <p class="text-2 check-why">{{ c.why }}</p>
        <p v-if="c.hint" class="small muted">{{ c.hint }}</p>
        <div v-if="c.id === 'ufw' && sec.data.value?.ufw.installed" class="rules">
          <span class="lbl">Rules</span>
          <pre v-if="sec.data.value.ufw.rules.length" class="logbox">{{ sec.data.value.ufw.rules.join('\n') }}</pre>
          <span v-else class="small muted">No rules.</span>
        </div>
      </div>
    </div>
  </section>

  <section class="card card-pad stack" aria-labelledby="blk-h" style="gap: 12px">
    <div class="row" style="gap: 10px"><AppIcon name="shield" :size="18" /><h2 id="blk-h" style="font-size: 17px; font-weight: 600">Block IPs on every site</h2></div>
    <p class="small muted">Addresses here get "403 Forbidden" on every website nginx serves. The panel stays reachable. One per line; ranges like <span class="mono">203.0.113.0/24</span> work. Block one site only from its project's Settings.</p>
    <textarea v-model="blockText" class="in mono" rows="5" placeholder="203.0.113.7&#10;198.51.100.0/24&#10;2001:db8::/32" aria-label="Blocked addresses" style="min-height: 110px; padding: 10px; resize: vertical" />
    <p v-if="blockErr" class="small" style="color: var(--bad)" role="alert">{{ blockErr }}</p>
    <p v-else-if="blockMsg" class="small" style="color: var(--ok)" role="status">{{ blockMsg }}</p>
    <button type="button" class="btn btn-p" style="align-self: flex-start" :disabled="blockBusy" @click="saveBlocklist">{{ blockBusy ? 'Saving…' : 'Save block list' }}</button>
  </section>

  <section class="card" aria-labelledby="sftp-h" style="overflow: hidden">
    <div class="row" style="gap: 10px; padding: 16px 20px"><AppIcon name="folder" :size="18" /><h2 id="sftp-h" style="font-size: 17px; font-weight: 600">SFTP accounts · {{ sftpAll.data.value?.length ?? 0 }}</h2></div>
    <div style="overflow-x: auto">
      <table>
        <thead><tr><th>Account</th><th>Site</th><th>Folder</th><th>Login</th></tr></thead>
        <tbody>
          <tr v-for="u in sftpAll.data.value ?? []" :key="u.username">
            <td class="mono">{{ u.username }}</td>
            <td><NuxtLink :to="`/projects/${encodeURIComponent(u.site_id)}?tab=settings`">{{ u.site_id }}</NuxtLink></td>
            <td><span :class="`pill ${u.mounted ? 'ok' : 'warn'}`">{{ u.mounted ? u.dir : 'not mounted' }}</span></td>
            <td class="small">{{ u.has_key ? 'password / SSH key' : 'password' }}</td>
          </tr>
          <tr v-if="!(sftpAll.data.value ?? []).length"><td colspan="4" class="muted">No SFTP accounts. Add one from a project's Settings tab.</td></tr>
        </tbody>
      </table>
    </div>
  </section>

  <section class="card" aria-labelledby="mal-h" style="overflow: hidden">
    <div style="display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px; padding: 16px 20px">
      <div style="display: flex; flex-direction: column; gap: 4px">
        <div class="row" style="gap: 10px"><AppIcon name="search" :size="18" /><h2 id="mal-h" style="font-size: 17px; font-weight: 600">Malware scan</h2></div>
        <span class="small muted">ClamAV checks site files for web shells and known malware. <template v-if="mal.data.value?.installed">Signatures updated {{ mal.data.value.signatures_updated ? formatRelative(new Date(mal.data.value.signatures_updated * 1000).toISOString()) : 'never' }}.</template></span>
      </div>
      <div v-if="mal.data.value?.installed" class="row" style="gap: 8px; flex-wrap: wrap">
        <div style="width: 220px"><SearchSelect v-model="malPath" :options="scanTargets" allow-custom mono placeholder="/var/www" aria-label="Folder to scan" /></div>
        <button type="button" class="btn btn-p" :disabled="!malPath" @click="malStart('/api/malware/scan', { path: malPath })">Scan now</button>
      </div>
      <button v-else type="button" class="btn btn-p" @click="malStart('/api/malware/install')"><AppIcon name="download" :size="14" /> Install ClamAV</button>
    </div>
    <p v-if="malErr" class="small" style="color: var(--bad); padding: 0 20px 12px" role="alert">{{ malErr }}</p>
    <div v-if="malJob" style="padding: 0 20px 16px"><JobLog :key="malJob" :job-id="malJob" status="running" @finished="mal.refresh()" /></div>
    <template v-if="mal.data.value?.last">
      <div style="padding: 0 20px 12px" class="small muted">Last scan of <span class="mono">{{ mal.data.value.last.path }}</span> {{ formatRelative(mal.data.value.last.finished_at) }} · {{ mal.data.value.last.scanned_files }} files · <span :style="`color: ${mal.data.value.last.found.length ? 'var(--bad)' : 'var(--ok)'}`">{{ mal.data.value.last.found.length ? `${mal.data.value.last.found.length} infected` : 'nothing found' }}</span></div>
      <div v-if="mal.data.value.last.found.length" style="overflow-x: auto">
        <table>
          <thead><tr><th>File</th><th>Found</th><th><span class="sr-only">Actions</span></th></tr></thead>
          <tbody>
            <tr v-for="f in mal.data.value.last.found" :key="f.file">
              <td class="mono" style="font-size: 13px; white-space: normal; word-break: break-all">{{ f.file }}</td>
              <td><span class="pill bad">{{ f.signature }}</span></td>
              <td><ConfirmAction label="Quarantine" confirm-label="Move to quarantine" question="Move this file out of the site?" @confirm="quarantineFile(f.file)" /></td>
            </tr>
          </tbody>
        </table>
      </div>
    </template>
    <p v-if="mal.data.value?.quarantine.length" class="small muted" style="padding: 0 20px 16px">{{ mal.data.value.quarantine.length }} file(s) in quarantine (/var/lib/nexacloud/quarantine, root only).</p>
  </section>
</template>

<style scoped>
.check { display: flex; gap: 14px; padding: 18px 20px; border-bottom: 1px solid var(--row-line); }
.check:last-child { border-bottom: 0; }
.check-icon { width: 32px; height: 32px; border-radius: 8px; display: grid; place-items: center; flex: none; }
.check-title { font-size: 16px; }
.check-why { font-size: 13px; line-height: 1.5; max-width: 72ch; }
.rules { display: flex; flex-direction: column; gap: 6px; margin-top: 8px; }
.rules .logbox { max-height: 240px; }
.fix { border-color: #24405A; }
</style>
