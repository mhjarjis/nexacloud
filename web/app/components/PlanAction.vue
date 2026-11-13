<script setup lang="ts">
// One server change, end to end: Preview → (diffs, commands, checks) → Apply →
// live log → result with Rollback. Talks to /api/plans/{preview,apply}.
import type { ChangePreviewData, ChangeStarted, ChangeSummary, JobStatus } from '~/types/api'

const props = withDefaults(
  defineProps<{
    /** `{ kind, params }` sent to /api/plans/*. */
    request: { kind: string; params: Record<string, unknown> }
    label: string
    applyLabel?: string
    /** Text shown after a successful apply. */
    successText?: string
    btnClass?: string
    disabled?: boolean
    icon?: string
  }>(),
  { applyLabel: 'Apply', successText: 'Done.', btnClass: 'btn btn-s', disabled: false, icon: '' },
)
const emit = defineEmits<{ done: [status: string]; open: []; close: [] }>()

const api = useApi()
type Step = 'idle' | 'previewing' | 'preview' | 'applying' | 'running' | 'result'
const step = ref<Step>('idle')
const preview = ref<ChangePreviewData | null>(null)
const started = ref<ChangeStarted | null>(null)
const result = ref<ChangeSummary | null>(null)
const error = ref<string | null>(null)

async function doPreview() {
  step.value = 'previewing'
  error.value = null
  emit('open')
  try {
    preview.value = await api.post<ChangePreviewData>('/api/plans/preview', props.request as unknown as Record<string, unknown>)
    step.value = 'preview'
  } catch (e) {
    error.value = errorMessage(e)
    step.value = 'idle'
  }
}

async function doApply() {
  if (!preview.value) return
  step.value = 'applying'
  error.value = null
  try {
    started.value = await api.post<ChangeStarted>('/api/plans/apply', {
      ...(props.request as unknown as Record<string, unknown>),
      plan_hash: preview.value.plan_hash,
    })
    step.value = 'running'
  } catch (e) {
    step.value = 'preview'
    if (!(e instanceof SudoCancelled)) error.value = errorMessage(e)
  }
}

async function onFinished(_s: JobStatus) {
  if (!started.value) return
  try {
    result.value = await api.get<ChangeSummary>(`/api/changes/${encodeURIComponent(started.value.change_id)}`)
  } catch (e) {
    error.value = errorMessage(e)
  }
  step.value = 'result'
  emit('done', result.value?.status ?? 'failed')
}

function reset() {
  step.value = 'idle'
  preview.value = null
  started.value = null
  result.value = null
  error.value = null
  emit('close')
}
defineExpose({ start: doPreview, reset })
</script>

<template>
  <div class="plan-action stack" style="gap: 12px">
    <div v-if="step === 'idle' || step === 'previewing'" class="row wrap" style="gap: 10px">
      <button type="button" :class="btnClass" :disabled="disabled || step === 'previewing'" @click="doPreview">
        <span v-if="step === 'previewing'" class="spinner" aria-hidden="true" />
        <AppIcon v-else-if="icon" :name="icon as any" :size="14" :stroke="2" />
        {{ label }}
      </button>
    </div>

    <div v-if="(step === 'preview' || step === 'applying') && preview" class="card pa-box">
      <div class="row" style="justify-content: space-between; gap: 10px; margin-bottom: 10px">
        <strong>{{ preview.title }}</strong>
        <button type="button" class="btn btn-s btn-ghost" @click="reset">Cancel</button>
      </div>
      <ChangePreview :preview="preview" :applying="step === 'applying'" :apply-label="applyLabel" @apply="doApply" @cancel="reset" />
    </div>

    <div v-if="step === 'running' && started" class="card pa-box stack" style="gap: 10px">
      <strong>{{ preview?.title }}</strong>
      <div class="progress" role="progressbar" aria-label="Applying change" aria-busy="true"><span /></div>
      <JobLog :job-id="started.job_id" status="running" @finished="onFinished" />
    </div>

    <div v-if="step === 'result'" class="card pa-box stack" style="gap: 8px">
      <div class="progress" :class="result?.status === 'applied' ? 'done' : 'failed'"><span /></div>
      <p v-if="result?.status === 'applied'" style="color: var(--ok)">{{ successText }}</p>
      <p v-else-if="result?.status === 'reverted'" style="color: var(--warn)">
        It didn't pass its checks, so NexaCloud put everything back automatically: {{ result?.error }}
      </p>
      <p v-else style="color: var(--bad)">{{ result?.error ?? 'The change did not apply.' }}</p>
      <div class="row wrap" style="gap: 12px">
        <NuxtLink v-if="result?.can_rollback" :to="`/activity?open=${result.id}`" class="btn btn-s">
          <AppIcon name="undo" :size="14" :stroke="2" /> Roll back
        </NuxtLink>
        <NuxtLink v-if="result" :to="`/activity?open=${result.id}`" class="small">View in Activity</NuxtLink>
        <button type="button" class="btn btn-s btn-ghost" @click="reset">Close</button>
      </div>
    </div>

    <p v-if="error" class="small" style="color: var(--bad)" role="alert">{{ error }}</p>
  </div>
</template>

<style scoped>
.pa-box { padding: 16px 18px; background: var(--sunken, #0f1318); }
</style>
