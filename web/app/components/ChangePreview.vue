<script setup lang="ts">
// What a change will do, before it is applied: file diffs, commands, checks,
// and how it is undone. Emits `apply`; the parent owns the API call.
import type { ChangePreviewData, VerifyStep } from '~/types/api'

const props = withDefaults(
  defineProps<{
    preview: ChangePreviewData
    applying?: boolean
    applyLabel?: string
    /** Hide the Apply button (e.g. read-only views). */
    readonly?: boolean
  }>(),
  { applying: false, applyLabel: 'Apply change', readonly: false },
)
const emit = defineEmits<{ apply: []; cancel: [] }>()

const nFiles = computed(() => props.preview.files.length)
const nCmds = computed(() => props.preview.commands.length)
const summary = computed(() => {
  const f = `${nFiles.value} file${nFiles.value === 1 ? '' : 's'}`
  const c = `${nCmds.value} command${nCmds.value === 1 ? '' : 's'}`
  return `This will change ${f} and run ${c}.`
})

const ACTIONS: Record<string, string> = { create: 'New file', modify: 'Modified', delete: 'Deleted', permissions: 'Permissions' }

function stepText(v: VerifyStep): string {
  return v.kind === 'http' ? `GET ${v.url} answers ${v.expect_status}` : `${displayArgv(v.argv)} succeeds`
}
</script>

<template>
  <div class="change-preview stack" style="gap: 14px">
    <template v-if="preview.nothing_to_do">
      <div class="row wrap" style="gap: 10px">
        <StatusPill tone="ok" label="Nothing to do" />
        <span class="text-2">{{ preview.note ?? 'The server already looks the way this change would make it.' }}</span>
      </div>
    </template>
    <template v-else>
      <p class="cp-summary">{{ summary }}</p>
      <p v-if="preview.note" class="small" style="color: var(--warn)">{{ preview.note }}</p>

      <div v-if="preview.files.length" class="stack" style="gap: 8px">
        <span class="lbl">Files</span>
        <details v-for="f in preview.files" :key="f.path" class="cp-file" open>
          <summary>
            <AppIcon name="chevron" :size="14" :stroke="2" class="chev muted" />
            <span class="mono cp-path">{{ f.path }}</span>
            <span class="tag">{{ ACTIONS[f.action] ?? f.action }}</span>
            <span v-if="f.mode" class="small muted mono">{{ f.mode }}</span>
          </summary>
          <DiffView v-if="f.diff" :diff="f.diff" />
        </details>
      </div>

      <div v-if="preview.commands.length" class="stack" style="gap: 6px">
        <span class="lbl">Then run</span>
        <ol class="cp-list mono">
          <li v-for="(c, i) in preview.commands" :key="i">{{ displayArgv(c) }}</li>
        </ol>
      </div>

      <div v-if="preview.verify.length" class="stack" style="gap: 6px">
        <span class="lbl">Check that</span>
        <ul class="cp-list">
          <li v-for="(v, i) in preview.verify" :key="i"><span class="mono">{{ stepText(v) }}</span></li>
        </ul>
      </div>

      <p class="small muted">
        Every file is snapshotted first. If a command or check fails, NexaCloud puts the files back<template v-if="preview.on_revert.length"> and runs <span class="mono">{{ preview.on_revert.map(displayArgv).join(', ') }}</span></template>
        automatically. You can roll back later from Activity.
      </p>

      <div v-if="!readonly" class="row wrap" style="gap: 8px">
        <button type="button" class="btn btn-p" :disabled="applying" @click="emit('apply')">
          <span v-if="applying" class="spinner" aria-hidden="true" />
          {{ applyLabel }}
        </button>
        <button type="button" class="btn btn-ghost" :disabled="applying" @click="emit('cancel')">Cancel</button>
      </div>
    </template>
  </div>
</template>

<style scoped>
.cp-summary { font-size: 15px; font-weight: 600; }
.cp-file { border: 1px solid var(--row-line); border-radius: 10px; background: var(--sunken); }
.cp-file > summary {
  display: flex; align-items: center; gap: 10px; flex-wrap: wrap; padding: 10px 12px; cursor: pointer; list-style: none;
}
.cp-file > summary::-webkit-details-marker { display: none; }
.cp-file[open] > summary { border-bottom: 1px solid var(--row-line); }
.cp-file :deep(.diff) { border: 0; border-radius: 0 0 10px 10px; }
.cp-path { font-size: 13px; overflow-wrap: anywhere; min-width: 0; }
.chev { transition: transform .15s ease; flex: none; }
.cp-file[open] .chev { transform: rotate(90deg); }
.cp-list { margin: 0; padding-left: 22px; display: flex; flex-direction: column; gap: 4px; font-size: 13px; color: var(--text-2); }
.cp-list.mono { font-family: var(--font-mono); font-size: 12px; }
</style>
