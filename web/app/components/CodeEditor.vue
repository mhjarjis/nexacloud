<script setup lang="ts">
// Code editor (CodeMirror 6): syntax highlighting picked from the file name,
// line numbers, search & replace (Ctrl/⌘+F), auto-indent, bracket matching,
// code folding, multiple cursors. Ctrl/⌘+S emits `save`.
import type { EditorView } from '@codemirror/view'
import type { Extension } from '@codemirror/state'

const props = withDefaults(defineProps<{
  modelValue: string
  filename?: string
  readonly?: boolean
  height?: string
}>(), { filename: '', readonly: false, height: '560px' })
const emit = defineEmits<{ 'update:modelValue': [v: string]; save: []; cursor: [line: number, col: number] }>()

const host = ref<HTMLElement | null>(null)
let view: EditorView | null = null
let applyingOuter = false
const loading = ref(true)
const langName = ref('Plain text')

async function language(name: string): Promise<{ ext: Extension | null; label: string }> {
  const n = name.toLowerCase()
  const ext = n.includes('.') ? n.slice(n.lastIndexOf('.') + 1) : ''
  if (n.endsWith('.blade.php')) { const m = await import('@codemirror/lang-php'); return { ext: m.php({ plain: false }), label: 'Blade / PHP' } }
  switch (ext) {
    case 'php': case 'phtml': { const m = await import('@codemirror/lang-php'); return { ext: m.php(), label: 'PHP' } }
    case 'js': case 'mjs': case 'cjs': { const m = await import('@codemirror/lang-javascript'); return { ext: m.javascript(), label: 'JavaScript' } }
    case 'ts': case 'mts': { const m = await import('@codemirror/lang-javascript'); return { ext: m.javascript({ typescript: true }), label: 'TypeScript' } }
    case 'jsx': case 'tsx': { const m = await import('@codemirror/lang-javascript'); return { ext: m.javascript({ jsx: true, typescript: ext === 'tsx' }), label: ext.toUpperCase() } }
    case 'vue': case 'html': case 'htm': { const m = await import('@codemirror/lang-html'); return { ext: m.html(), label: 'HTML' } }
    case 'css': case 'scss': case 'less': { const m = await import('@codemirror/lang-css'); return { ext: m.css(), label: 'CSS' } }
    case 'json': case 'lock': { const m = await import('@codemirror/lang-json'); return { ext: m.json(), label: 'JSON' } }
    case 'md': case 'markdown': { const m = await import('@codemirror/lang-markdown'); return { ext: m.markdown(), label: 'Markdown' } }
    case 'sql': { const m = await import('@codemirror/lang-sql'); return { ext: m.sql(), label: 'SQL' } }
    case 'xml': case 'svg': { const m = await import('@codemirror/lang-xml'); return { ext: m.xml(), label: 'XML' } }
    case 'yml': case 'yaml': { const m = await import('@codemirror/lang-yaml'); return { ext: m.yaml(), label: 'YAML' } }
  }
  const { StreamLanguage } = await import('@codemirror/language')
  if (ext === 'sh' || ext === 'bash' || n.endsWith('/artisan') || n === 'artisan') {
    const m = await import('@codemirror/legacy-modes/mode/shell'); return { ext: StreamLanguage.define(m.shell), label: 'Shell' }
  }
  if (ext === 'conf' || n.includes('nginx')) {
    const m = await import('@codemirror/legacy-modes/mode/nginx'); return { ext: StreamLanguage.define(m.nginx), label: 'nginx' }
  }
  if (ext === 'ini' || ext === 'env' || n.startsWith('.env') || n.includes('/.env') || ext === 'example' || ext === 'toml' || ext === 'cnf') {
    const m = await import('@codemirror/legacy-modes/mode/properties'); return { ext: StreamLanguage.define(m.properties), label: ext === 'toml' ? 'TOML' : '.env / INI' }
  }
  if (n.endsWith('dockerfile')) {
    const m = await import('@codemirror/legacy-modes/mode/dockerfile'); return { ext: StreamLanguage.define(m.dockerFile), label: 'Dockerfile' }
  }
  return { ext: null, label: 'Plain text' }
}

onMounted(async () => {
  const [{ EditorView, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter, drawSelection, rectangularSelection, crosshairCursor, highlightSpecialChars, dropCursor }, { EditorState }, cmd, lang, search, { tags: t }] = await Promise.all([
    import('@codemirror/view'), import('@codemirror/state'), import('@codemirror/commands'), import('@codemirror/language'), import('@codemirror/search'), import('@lezer/highlight'),
  ])
  const { ext, label } = await language(props.filename)
  langName.value = label

  const theme = EditorView.theme({
    '&': { color: '#E7ECF2', backgroundColor: '#07090C', height: props.height, fontSize: '13px' },
    '.cm-scroller': { fontFamily: "'JetBrains Mono', ui-monospace, Menlo, monospace", lineHeight: '1.6' },
    '.cm-content': { caretColor: '#3DDC97', padding: '10px 0' },
    '.cm-cursor, .cm-dropCursor': { borderLeftColor: '#3DDC97', borderLeftWidth: '2px' },
    '&.cm-focused .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection': { backgroundColor: '#2A3F55 !important' },
    '.cm-gutters': { backgroundColor: '#0B0E12', color: '#5B6573', border: 'none', borderRight: '1px solid #1B222B' },
    '.cm-activeLineGutter': { backgroundColor: '#12171D', color: '#C5CEDA' },
    '.cm-activeLine': { backgroundColor: 'rgba(255,255,255,.035)' },
    '.cm-foldPlaceholder': { backgroundColor: '#1F2833', border: 'none', color: '#A3AEBD' },
    '.cm-matchingBracket': { backgroundColor: 'rgba(61,220,151,.18)', outline: '1px solid rgba(61,220,151,.45)' },
    '.cm-searchMatch': { backgroundColor: 'rgba(247,197,94,.25)', outline: '1px solid rgba(247,197,94,.5)' },
    '.cm-searchMatch.cm-searchMatch-selected': { backgroundColor: 'rgba(247,197,94,.5)' },
    '.cm-selectionMatch': { backgroundColor: 'rgba(108,180,255,.15)' },
    '.cm-panels': { backgroundColor: '#12171D', color: '#E7ECF2', borderTop: '1px solid #222A34' },
    '.cm-panels.cm-panels-top': { borderBottom: '1px solid #222A34' },
    '.cm-panel input, .cm-panel button': { fontSize: '12px' },
    '.cm-textfield': { backgroundColor: '#0F1318', border: '1px solid #2A3441', color: '#E7ECF2', borderRadius: '6px', padding: '3px 6px' },
    '.cm-button': { backgroundImage: 'none', backgroundColor: '#1F2833', border: '1px solid #2A3441', color: '#E7ECF2', borderRadius: '6px' },
    '.cm-tooltip': { backgroundColor: '#12171D', border: '1px solid #2A3441', color: '#E7ECF2' },
  }, { dark: true })
  const highlight = lang.HighlightStyle.define([
    { tag: [t.keyword, t.modifier, t.controlKeyword, t.operatorKeyword], color: '#C792EA' },
    { tag: [t.string, t.special(t.string), t.regexp], color: '#A5E07A' },
    { tag: [t.number, t.bool, t.null, t.atom], color: '#F7A26C' },
    { tag: [t.comment, t.lineComment, t.blockComment, t.docComment], color: '#6A7686', fontStyle: 'italic' },
    { tag: [t.variableName, t.special(t.variableName)], color: '#E7ECF2' },
    { tag: [t.definition(t.variableName), t.function(t.variableName), t.function(t.propertyName)], color: '#6CB4FF' },
    { tag: [t.propertyName, t.attributeName], color: '#7FE0D4' },
    { tag: [t.typeName, t.className, t.namespace], color: '#F7C55E' },
    { tag: [t.tagName, t.angleBracket], color: '#FF9A9A' },
    { tag: [t.operator, t.punctuation, t.separator], color: '#A3AEBD' },
    { tag: [t.heading], color: '#6CB4FF', fontWeight: '700' },
    { tag: [t.link, t.url], color: '#6CB4FF', textDecoration: 'underline' },
    { tag: [t.meta, t.processingInstruction], color: '#8A96A8' },
    { tag: t.invalid, color: '#FF6B6B' },
  ])

  const exts: Extension[] = [
    lineNumbers(), highlightActiveLineGutter(), highlightSpecialChars(), cmd.history(), lang.foldGutter(), drawSelection(), dropCursor(),
    EditorState.allowMultipleSelections.of(true), lang.indentOnInput(), lang.bracketMatching(), rectangularSelection(), crosshairCursor(),
    highlightActiveLine(), search.highlightSelectionMatches(), lang.indentUnit.of('    '), EditorState.tabSize.of(4),
    keymap.of([
      { key: 'Mod-s', preventDefault: true, run: () => { emit('save'); return true } },
      cmd.indentWithTab,
      ...cmd.defaultKeymap, ...cmd.historyKeymap, ...lang.foldKeymap, ...search.searchKeymap,
    ]),
    theme, lang.syntaxHighlighting(highlight), lang.syntaxHighlighting(lang.defaultHighlightStyle, { fallback: true }),
    EditorState.readOnly.of(props.readonly), EditorView.editable.of(!props.readonly),
    EditorView.updateListener.of((u) => {
      if (u.docChanged && !applyingOuter) emit('update:modelValue', u.state.doc.toString())
      if (u.selectionSet || u.docChanged) {
        const pos = u.state.selection.main.head
        const line = u.state.doc.lineAt(pos)
        emit('cursor', line.number, pos - line.from + 1)
      }
    }),
  ]
  if (ext) exts.push(ext)
  if (!host.value) return
  view = new EditorView({ parent: host.value, state: EditorState.create({ doc: props.modelValue, extensions: exts }) })
  loading.value = false
  view.focus()
})

// content replaced from outside (e.g. after reload)
watch(() => props.modelValue, (v) => {
  if (!view || v === view.state.doc.toString()) return
  applyingOuter = true
  view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: v } })
  applyingOuter = false
})

onBeforeUnmount(() => { view?.destroy(); view = null })
defineExpose({ language: langName })
</script>

<template>
  <div class="code-editor">
    <div ref="host" />
    <div v-if="loading" class="mono" style="padding: 16px 20px; font-size: 13px; color: #A3AEBD">Loading editor…</div>
  </div>
</template>

<style scoped>
.code-editor :deep(.cm-editor) { outline: none; }
</style>
