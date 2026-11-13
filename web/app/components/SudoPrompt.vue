<script setup lang="ts">
// Inline "confirm it's you" panel, mounted once in the layout. Shown when an
// API call answered 403 sudo_required; useApi() retries the call after the
// password is accepted (see composables/useSudo.ts).
const { state, submit, cancel } = useSudo()
const password = ref('')
const input = ref<HTMLInputElement | null>(null)
const root = ref<HTMLElement | null>(null)

watch(
  () => state.value.open,
  async (open) => {
    password.value = ''
    if (!open) return
    await nextTick()
    root.value?.scrollIntoView({ block: 'nearest', behavior: 'smooth' })
    input.value?.focus()
  },
)

async function onSubmit() {
  const ok = await submit(password.value)
  password.value = ''
  if (!ok) {
    await nextTick()
    input.value?.focus()
  }
}
</script>

<template>
  <section
    v-if="state.open"
    ref="root"
    class="card sudo-prompt"
    role="dialog"
    aria-modal="false"
    aria-labelledby="sudo-h"
    @keydown.esc="cancel()"
  >
    <div class="row" style="gap: 12px; align-items: flex-start">
      <div class="sudo-icon" aria-hidden="true"><AppIcon name="lock" :size="16" :stroke="2" /></div>
      <div class="grow stack" style="gap: 10px">
        <div class="stack" style="gap: 2px">
          <h2 id="sudo-h" class="sudo-title">Confirm it's you</h2>
          <p class="small text-2">This action changes the server. Enter the admin password to continue; you won't be asked again for 15 minutes.</p>
        </div>
        <form class="row wrap" style="gap: 8px" novalidate @submit.prevent="onSubmit">
          <label for="sudo-pw" class="sr-only">Admin password</label>
          <input
            id="sudo-pw"
            ref="input"
            v-model="password"
            class="in grow"
            type="password"
            autocomplete="current-password"
            placeholder="Admin password"
            :disabled="state.busy"
            style="min-width: 200px"
          >
          <button type="submit" class="btn btn-s btn-p" :disabled="!password || state.busy">
            <span v-if="state.busy" class="spinner" aria-hidden="true" />
            Confirm
          </button>
          <button type="button" class="btn btn-s btn-ghost" @click="cancel()">Cancel</button>
        </form>
        <p v-if="state.error" class="small" style="color: var(--bad)" role="alert">{{ state.error }}</p>
      </div>
    </div>
  </section>
</template>

<style scoped>
.sudo-prompt {
  position: sticky; top: 12px; z-index: 20; padding: 16px 18px;
  border-color: #5A4A22; background: #17140C; box-shadow: 0 8px 28px rgba(0, 0, 0, .45);
}
.sudo-icon { width: 32px; height: 32px; border-radius: 8px; display: grid; place-items: center; flex: none; background: var(--warn-bg); color: var(--warn); }
.sudo-title { font-size: 16px; }
</style>
