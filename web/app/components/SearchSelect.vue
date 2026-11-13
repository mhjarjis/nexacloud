<script setup lang="ts">
// Searchable select (combobox). Type to filter, ↑/↓ to move, Enter to pick,
// Esc to close. Options: strings, or { value, label, hint?, disabled? }.
type Opt = string | number | { value: string | number | null; label: string; hint?: string; disabled?: boolean }

const props = withDefaults(defineProps<{
  modelValue: string | number | null | undefined
  options: Opt[]
  placeholder?: string
  ariaLabel?: string
  disabled?: boolean
  mono?: boolean
  /** Allow a value that isn't in the list (typed by the person). */
  allowCustom?: boolean
}>(), { placeholder: 'Search…', ariaLabel: undefined, disabled: false, mono: false, allowCustom: false })
const emit = defineEmits<{ 'update:modelValue': [v: string | number | null]; change: [v: string | number | null] }>()

const norm = computed(() => props.options.map((o) =>
  typeof o === 'object' && o !== null ? o : { value: o as string | number, label: String(o) },
))
const selected = computed(() => norm.value.find((o) => o.value === props.modelValue))
const open = ref(false)
const query = ref('')
const active = ref(0)
const root = ref<HTMLElement | null>(null)
const input = ref<HTMLInputElement | null>(null)
const uid = `ss-${Math.random().toString(36).slice(2, 9)}`

const filtered = computed(() => {
  const q = query.value.trim().toLowerCase()
  if (!q) return norm.value
  return norm.value.filter((o) => o.label.toLowerCase().includes(q) || String(o.value ?? '').toLowerCase().includes(q) || (o.hint ?? '').toLowerCase().includes(q))
})
const display = computed(() => (open.value ? query.value : (selected.value?.label ?? (props.allowCustom && props.modelValue != null ? String(props.modelValue) : ''))))

function openList() {
  if (props.disabled) return
  open.value = true
  query.value = ''
  active.value = Math.max(0, filtered.value.findIndex((o) => o.value === props.modelValue))
}
function pick(o: { value: string | number | null; disabled?: boolean }) {
  if (o.disabled) return
  emit('update:modelValue', o.value)
  emit('change', o.value)
  open.value = false
  input.value?.blur()
}
function onKey(e: KeyboardEvent) {
  if (!open.value && ['ArrowDown', 'Enter'].includes(e.key)) { openList(); e.preventDefault(); return }
  if (e.key === 'ArrowDown') { active.value = Math.min(active.value + 1, filtered.value.length - 1); e.preventDefault() }
  else if (e.key === 'ArrowUp') { active.value = Math.max(active.value - 1, 0); e.preventDefault() }
  else if (e.key === 'Enter') {
    e.preventDefault()
    const o = filtered.value[active.value]
    if (o) pick(o)
    else if (props.allowCustom && query.value.trim()) pick({ value: query.value.trim() })
  } else if (e.key === 'Escape') { open.value = false }
}
function onInput(e: Event) {
  query.value = (e.target as HTMLInputElement).value
  open.value = true
  active.value = 0
}
function onDocClick(e: MouseEvent) {
  if (open.value && root.value && !root.value.contains(e.target as Node)) {
    if (props.allowCustom && query.value.trim()) pick({ value: query.value.trim() })
    open.value = false
  }
}
onMounted(() => document.addEventListener('mousedown', onDocClick))
onBeforeUnmount(() => document.removeEventListener('mousedown', onDocClick))
watch(active, async (i) => {
  await nextTick()
  root.value?.querySelector(`#${uid}-o${i}`)?.scrollIntoView({ block: 'nearest' })
})
</script>

<template>
  <div ref="root" class="ss" :class="{ open, disabled }">
    <input
      ref="input"
      class="in ss-in"
      :class="{ mono }"
      type="text"
      role="combobox"
      :aria-label="ariaLabel"
      :aria-expanded="open"
      :aria-controls="`${uid}-list`"
      :aria-activedescendant="open && filtered.length ? `${uid}-o${active}` : undefined"
      aria-autocomplete="list"
      autocomplete="off"
      spellcheck="false"
      :placeholder="selected ? selected.label : placeholder"
      :value="display"
      :disabled="disabled"
      @focus="openList"
      @click="openList"
      @input="onInput"
      @keydown="onKey"
    >
    <svg class="ss-chev" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M6 9l6 6 6-6" /></svg>
    <ul v-if="open" :id="`${uid}-list`" class="ss-list" role="listbox">
      <li
        v-for="(o, i) in filtered"
        :id="`${uid}-o${i}`"
        :key="String(o.value) + i"
        role="option"
        :aria-selected="o.value === modelValue"
        :aria-disabled="o.disabled || undefined"
        :class="{ act: i === active, sel: o.value === modelValue, dis: o.disabled }"
        @mousedown.prevent="pick(o)"
        @mousemove="active = i"
      >
        <span :class="{ mono }">{{ o.label }}</span>
        <span v-if="o.hint" class="ss-hint">{{ o.hint }}</span>
        <svg v-if="o.value === modelValue" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" aria-hidden="true"><path d="M20 6L9 17l-5-5" /></svg>
      </li>
      <li v-if="!filtered.length" class="ss-empty">{{ allowCustom && query.trim() ? `Press Enter to use “${query.trim()}”` : 'No matches' }}</li>
    </ul>
  </div>
</template>

<style scoped>
.ss { position: relative; min-width: 160px; }
.ss-in { width: 100%; padding-right: 34px !important; cursor: pointer; }
.ss.open .ss-in { cursor: text; border-color: var(--accent); }
.ss-chev { position: absolute; right: 10px; top: 50%; transform: translateY(-50%); color: var(--muted); pointer-events: none; transition: transform .15s; }
.ss.open .ss-chev { transform: translateY(-50%) rotate(180deg); }
.ss-list { position: absolute; z-index: 50; left: 0; right: 0; top: calc(100% + 4px); margin: 0; padding: 4px; list-style: none; max-height: 280px; overflow: auto; background: #151B22; border: 1px solid #2A3441; border-radius: 10px; box-shadow: 0 12px 32px rgba(0, 0, 0, .45); }
.ss-list li { display: flex; align-items: center; gap: 8px; padding: 8px 10px; border-radius: 7px; font-size: 13px; cursor: pointer; color: var(--text); min-height: 36px; box-sizing: border-box; }
.ss-list li.act { background: #1F2833; }
.ss-list li.sel { color: var(--accent); }
.ss-list li.dis { opacity: .5; cursor: not-allowed; }
.ss-list li > span:first-child { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.ss-hint { font-size: 12px; color: var(--muted); white-space: nowrap; }
.ss-empty { color: var(--muted) !important; cursor: default !important; }
.mono { font-family: var(--font-mono); }
</style>
