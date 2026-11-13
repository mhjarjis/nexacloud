<script setup lang="ts">
import type { SecurityInfo, Tone } from '~/types/api'

useHead({ title: 'Security · NexaCloud' })

const sec = useResource<SecurityInfo>('/api/security')

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

  <InfoNote>One-click fixes arrive in v0.2. For now each check tells you what it means and how to fix it by hand.</InfoNote>

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
</style>
