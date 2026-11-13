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
