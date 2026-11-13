<script setup lang="ts">
definePageMeta({ layout: 'bare' })
useHead({ title: 'Sign in · NexaCloud' })

const api = useApi()
const authed = useAuthState()
const route = useRoute()

const password = ref('')
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
  if (!password.value || busy.value) return
  busy.value = true
  error.value = null
  try {
    await api.request('/api/auth/login', {
      method: 'POST',
      body: { password: password.value },
      allowUnauthorized: true,
    })
    authed.value = true
    password.value = ''
    await navigateTo(nextPath())
  } catch (err) {
    const status = err instanceof ApiError ? err.status : 0
    if (status === 401) error.value = 'Wrong password. Try again.'
    else if (status === 429) error.value = 'Too many attempts from this address. Wait 15 minutes, then try again.'
    else error.value = errorMessage(err)
    password.value = ''
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
      <p class="muted">Use the admin password printed when NexaCloud was installed.</p>
    </div>

    <form class="stack" style="gap: 14px" novalidate @submit.prevent="submit">
      <label class="field">
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
        >
      </label>
      <p id="login-error" class="login-error" role="alert" aria-live="assertive">
        <template v-if="error">{{ error }}</template>
      </p>
      <button type="submit" class="btn btn-p" :disabled="busy || !password">
        <span v-if="busy" class="spinner" aria-hidden="true" />
        {{ busy ? 'Signing in…' : 'Sign in' }}
      </button>
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
