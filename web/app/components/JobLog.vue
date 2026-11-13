<script setup lang="ts">
// A job's output: live over SSE while it runs, the stored log once finished.
import type { JobStatus } from '~/types/api'

const props = defineProps<{ jobId: string; status: JobStatus; log?: string }>()
const emit = defineEmits<{ finished: [status: JobStatus] }>()

const live = computed(() => !isJobFinished(props.status))
const streamId = ref<string | null>(live.value ? props.jobId : null)
const stream = useJobStream(streamId)
const box = ref<HTMLElement | null>(null)

watch(() => stream.status.value, (s) => { if (s) emit('finished', s) })
watch(() => stream.lines.value.length, async () => {
  await nextTick()
  const el = box.value
  if (el && el.scrollHeight - el.scrollTop - el.clientHeight < 80) el.scrollTop = el.scrollHeight
})

const text = computed(() => (streamId.value && stream.lines.value.length ? stream.text.value : (props.log ?? '')))
</script>

<template>
  <div class="stack" style="gap: 6px">
    <div class="row" style="gap: 8px">
      <span class="lbl">Output</span>
      <span v-if="streamId && !stream.status.value" class="small muted row" style="gap: 6px">
        <span class="spinner" aria-hidden="true" style="width: 10px; height: 10px" /> live
      </span>
    </div>
    <pre v-if="text" ref="box" class="logbox job-log" aria-live="polite">{{ text }}</pre>
    <p v-else class="small muted">No output yet.</p>
  </div>
</template>
