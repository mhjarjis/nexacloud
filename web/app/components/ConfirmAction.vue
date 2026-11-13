<script setup lang="ts">
// Inline two-step confirmation: [Restart] → "Are you sure? [Restart] [Cancel]".
// Never uses window.confirm.
const props = withDefaults(
  defineProps<{
    label: string
    confirmLabel?: string
    question?: string
    busy?: boolean
    disabled?: boolean
    ariaLabel?: string
  }>(),
  { question: 'Are you sure?', busy: false, disabled: false },
)
const emit = defineEmits<{ confirm: [] }>()

const asking = ref(false)
const confirmBtn = ref<HTMLButtonElement | null>(null)
const triggerBtn = ref<HTMLButtonElement | null>(null)

async function ask() {
  asking.value = true
  await nextTick()
  confirmBtn.value?.focus()
}

async function cancel() {
  asking.value = false
  await nextTick()
  triggerBtn.value?.focus()
}

function confirm() {
  asking.value = false
  emit('confirm')
}
</script>

<template>
  <div class="confirm" @keydown.esc="asking && cancel()">
    <template v-if="!asking">
      <button
        ref="triggerBtn"
        type="button"
        class="btn btn-s"
        :disabled="props.disabled || props.busy"
        :aria-label="props.ariaLabel"
        @click="ask"
      >
        <span v-if="props.busy" class="spinner" aria-hidden="true" />
        <slot name="icon" />
        {{ props.label }}
      </button>
    </template>
    <div v-else class="confirm-ask" role="group" :aria-label="props.question">
      <span class="confirm-q">{{ props.question }}</span>
      <button ref="confirmBtn" type="button" class="btn btn-s btn-danger" @click="confirm">
        {{ props.confirmLabel ?? props.label }}
      </button>
      <button type="button" class="btn btn-s btn-ghost" @click="cancel">Cancel</button>
    </div>
  </div>
</template>

<style scoped>
.confirm { display: inline-flex; }
.confirm-ask { display: inline-flex; flex-wrap: wrap; align-items: center; gap: 6px; }
.confirm-q { font-size: 13px; color: var(--warn); font-weight: 500; margin-right: 2px; }
</style>
