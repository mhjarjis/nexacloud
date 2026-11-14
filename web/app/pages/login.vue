<script setup lang="ts">
definePageMeta({ layout: 'bare' })
useHead({ title: 'Sign in · NexaCloud' })

const api = useApi()
const authed = useAuthState()
const route = useRoute()

const username = ref('')
const password = ref('')
const code = ref('')
const needCode = ref(false)
const codeInput = ref<HTMLInputElement | null>(null)
const error = ref<string | null>(null)
const busy = ref(false)
const input = ref<HTMLInputElement | null>(null)

onMounted(() => input.value?.focus())

function nextPath(): string {
  const next = route.query.next
  // Only allow in-app paths.
  return typeof next === 'string' && next.startsWith('/') && !next.startsWith('//') ? next : '/'
}

async function submit() {
  if (!password.value || busy.value || (needCode.value && !code.value.trim())) return
  busy.value = true
  error.value = null
  try {
    await api.request('/api/auth/login', {
      method: 'POST',
      body: { username: username.value.trim() || undefined, password: password.value, code: needCode.value ? code.value.trim() : undefined },
      allowUnauthorized: true,
    })
    authed.value = true
    password.value = ''
    code.value = ''
    await navigateTo(nextPath())
  } catch (err) {
    const status = err instanceof ApiError ? err.status : 0
    const msg = err instanceof ApiError ? err.message : ''
    if (status === 401 && msg === 'totp_required') {
      needCode.value = true
      error.value = null
      await nextTick()
      codeInput.value?.focus()
      return
    }
    if (status === 401 && needCode.value && /two-factor/.test(msg)) {
      error.value = 'That code is not right. Use the current 6-digit code or a recovery code.'
      code.value = ''
      await nextTick()
      codeInput.value?.focus()
      return
    }
    if (status === 401) error.value = 'Wrong username or password. Try again.'
    else if (status === 429) error.value = 'Too many attempts from this address. Wait 15 minutes, then try again.'
    else error.value = errorMessage(err)
    password.value = ''
    needCode.value = false
    code.value = ''
    await nextTick()
    input.value?.focus()
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="login card">
    <div class="login-brand">
      <LogoMark :size="40" />
      <span class="brand-name">NexaCloud</span>
    </div>
    <div class="stack" style="gap: 6px; text-align: center">
      <h1 class="login-title">Sign in to your server</h1>
      <p class="muted">{{ needCode ? 'Enter the 6-digit code from your authenticator app.' : 'Admins: leave the username empty and use the password printed at install.' }}</p>
    </div>

    <form class="stack" style="gap: 14px" novalidate @submit.prevent="submit">
      <label v-show="!needCode" class="field">
        <span class="field-label">Username</span>
        <input v-model="username" class="in mono" type="text" name="username" autocomplete="username" placeholder="admin" spellcheck="false" autocapitalize="off">
      </label>
      <label v-show="!needCode" class="field">
        <span class="field-label">Password</span>
        <input
          ref="input"
          v-model="password"
          class="in mono"
          type="password"
          name="password"
          autocomplete="current-password"
          required
          :aria-invalid="!!error"
          aria-describedby="login-error"
          placeholder="Your password"
        >
      </label>
      <label v-if="needCode" class="field">
        <span class="field-label">Two-factor code</span>
        <input ref="codeInput" v-model="code" class="in mono" type="text" name="code" inputmode="numeric" autocomplete="one-time-code" placeholder="123 456 or a recovery code" spellcheck="false" maxlength="11" style="letter-spacing: 2px; text-align: center; font-size: 18px">
      </label>
      <p id="login-error" class="login-error" role="alert" aria-live="assertive">
        <template v-if="error">{{ error }}</template>
      </p>
      <button type="submit" class="btn btn-p" :disabled="busy || !password || (needCode && !code.trim())">
        <span v-if="busy" class="spinner" aria-hidden="true" />
        {{ busy ? 'Signing in…' : needCode ? 'Verify' : 'Sign in' }}
      </button>
      <button v-if="needCode" type="button" class="btn btn-s" @click="needCode = false; code = ''; password = ''">Back</button>
    </form>

    <p class="small muted" style="text-align: center">
      Forgot it? Run <code class="mono text-2">sudo nexacloud reset-password</code> on the server.
    </p>
  </div>
</template>

<style scoped>
.login { width: 100%; max-width: 400px; padding: 32px 28px; display: flex; flex-direction: column; gap: 22px; }
.login-brand { display: flex; align-items: center; justify-content: center; gap: 12px; }
.login-brand .brand-name { font-size: 22px; }
.login-title { font-size: 22px; }
.field { display: flex; flex-direction: column; gap: 6px; }
.field-label { font-size: 13px; font-weight: 500; color: var(--text-2); }
.field .in { min-height: 44px; width: 100%; }
.login-error { min-height: 18px; font-size: 13px; color: var(--bad); }
.login-error:empty { min-height: 0; }
</style>
