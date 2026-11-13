<script setup lang="ts">
// Unified diff, monospace, with added/removed lines tinted.
const props = defineProps<{ diff: string }>()

interface Line { text: string; kind: 'add' | 'del' | 'hunk' | 'head' | 'ctx' }

const lines = computed<Line[]>(() =>
  props.diff.replace(/\n$/, '').split('\n').map((text) => {
    if (text.startsWith('+++') || text.startsWith('---')) return { text, kind: 'head' }
    if (text.startsWith('@@')) return { text, kind: 'hunk' }
    if (text.startsWith('+')) return { text, kind: 'add' }
    if (text.startsWith('-')) return { text, kind: 'del' }
    return { text, kind: 'ctx' }
  }),
)
</script>

<template>
  <pre class="diff" aria-label="File changes"><code><span v-for="(l, i) in lines" :key="i" class="dl" :class="l.kind">{{ l.text || ' ' }}</span></code></pre>
</template>

<style scoped>
.diff {
  font-family: var(--font-mono); font-size: 12px; line-height: 1.6; margin: 0;
  background: var(--log-bg); border: 1px solid var(--row-line); border-radius: 10px;
  padding: 10px 0; overflow: auto; max-height: 420px; color: var(--text-2);
}
.dl { display: block; padding: 0 14px; white-space: pre; }
.dl.add { background: var(--ok-bg); color: var(--ok); }
.dl.del { background: var(--bad-bg); color: var(--bad); }
.dl.hunk { color: var(--info); }
.dl.head { color: var(--muted); }
</style>
