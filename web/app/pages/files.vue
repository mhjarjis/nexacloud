<script setup lang="ts">
useHead({ title: 'File manager · NexaCloud' })

interface Entry { name: string; kind: 'dir' | 'file'; size: number; mode: string; owner: string; modified: number; link: string | null }
interface Listing { path: string; writable: boolean; entries: Entry[] }
interface Roots { roots: { path: string; writable: boolean }[] }

const api = useApi()
const route = useRoute()
const router = useRouter()
const host = computed(() => (typeof window !== 'undefined' ? window.location.hostname : 'server'))

const path = computed(() => (typeof route.query.path === 'string' && route.query.path.startsWith('/') ? route.query.path.replace(/\/+$/, '') || '/' : ''))
const listing = ref<Listing | null>(null)
const roots = ref<Roots['roots']>([])
const loading = ref(false)
const err = ref('')
const msg = ref('')
const q = ref('')
const selected = ref<string[]>([])

async function load() {
  loading.value = true; err.value = ''; selected.value = []
  try {
    if (!path.value) {
      roots.value = (await api.get<Roots>('/api/files')).roots
      listing.value = null
    } else {
      listing.value = await api.get<Listing>(`/api/files?path=${encodeURIComponent(path.value)}`)
    }
  } catch (e) { err.value = errorMessage(e); listing.value = null } finally { loading.value = false }
}
watch(path, load, { immediate: true })

function go(p: string) { router.push({ query: p ? { path: p } : {} }); editor.open = false }
function join(name: string) { return path.value === '/' ? `/${name}` : `${path.value}/${name}` }
const crumbs = computed(() => {
  if (!path.value) return []
  const parts = path.value.split('/').filter(Boolean)
  return parts.map((p, i) => ({ name: p, path: '/' + parts.slice(0, i + 1).join('/') }))
})
const shown = computed(() => {
  const l = listing.value?.entries ?? []
  const s = q.value.trim().toLowerCase()
  return s ? l.filter((e) => e.name.toLowerCase().includes(s)) : l
})
const writable = computed(() => !!listing.value?.writable)
const allChecked = computed({
  get: () => shown.value.length > 0 && shown.value.every((e) => selected.value.includes(e.name)),
  set: (v: boolean) => { selected.value = v ? shown.value.map((e) => e.name) : [] },
})
const isArchive = (n: string) => /\.(zip|tar|tar\.gz|tgz|tar\.bz2|tar\.xz)$/i.test(n)
const TEXT = /\.(php|js|mjs|ts|vue|json|css|scss|html?|txt|md|env|ya?ml|xml|ini|conf|log|sh|sql|lock|htaccess|gitignore|example|blade\.php|svg|csv|toml)$|^\.env|^[^.]+$/i
function fmtTime(t: number) { return formatRelative(new Date(t * 1000).toISOString()) }

async function act(p: Promise<unknown>, done: string) {
  err.value = ''; msg.value = ''
  try { await p; msg.value = done; await load() } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) }
}

// ---- new file / folder / rename / chmod (inline prompt bar)
type Mode = '' | 'file' | 'folder' | 'rename' | 'chmod' | 'compress' | 'extract'
const bar = reactive({ mode: '' as Mode, value: '', target: '', recursive: false })
function openBar(mode: Mode, target = '', value = '') { Object.assign(bar, { mode, target, value, recursive: false }) }
async function submitBar() {
  const v = bar.value.trim()
  if (!v) return
  const m = bar.mode
  bar.mode = ''
  if (m === 'file') {
    await act(api.post('/api/files/write', { path: join(v), content: '' }), `Created ${v}`)
    openEditor(v)
  } else if (m === 'folder') await act(api.post('/api/files/mkdir', { path: join(v) }), `Created folder ${v}`)
  else if (m === 'rename') await act(api.post('/api/files/rename', { from: join(bar.target), to: v.startsWith('/') ? v : join(v) }), `Renamed to ${v}`)
  else if (m === 'chmod') await act(api.post('/api/files/chmod', { path: join(bar.target), mode: v, recursive: bar.recursive }), `Permissions set to ${v}`)
  else if (m === 'compress') await act(api.post('/api/files/compress', { paths: selected.value.map(join), dest: join(v) }), `Created ${v}`)
  else if (m === 'extract') await act(api.post('/api/files/extract', { path: join(bar.target), dest: v.startsWith('/') ? v : join(v) }), `Extracted ${bar.target}`)
}
const barLabel: Record<string, string> = { file: 'New file name', folder: 'New folder name', rename: 'New name', chmod: 'Permissions (e.g. 644 or 755)', compress: 'Archive name (.zip or .tar.gz)', extract: 'Extract into folder' }
const barPh: Record<string, string> = { file: 'notes.txt', folder: 'uploads', rename: 'new-name.php', chmod: '644', compress: 'backup.zip', extract: 'extracted' }

async function removeSelected() {
  await act(api.post('/api/files/delete', { paths: selected.value.map(join) }), `Deleted ${selected.value.length} item(s)`)
}

// ---- upload
const fileInput = ref<HTMLInputElement | null>(null)
const upload = reactive({ name: '', pct: 0, busy: false })
async function onPick(ev: Event) {
  const files = Array.from((ev.target as HTMLInputElement).files ?? [])
  ;(ev.target as HTMLInputElement).value = ''
  for (const f of files) {
    upload.name = f.name; upload.pct = 0; upload.busy = true
    try {
      await new Promise<void>((resolve, reject) => {
        const x = new XMLHttpRequest()
        x.open('POST', `/api/files/upload?dir=${encodeURIComponent(path.value)}&name=${encodeURIComponent(f.name)}`)
        x.withCredentials = true
        x.upload.onprogress = (e) => { if (e.lengthComputable) upload.pct = Math.round((e.loaded / e.total) * 100) }
        x.onload = () => {
          if (x.status >= 200 && x.status < 300) resolve()
          else { try { reject(new Error(JSON.parse(x.responseText).error)) } catch { reject(new Error(`Upload failed (HTTP ${x.status})`)) } }
        }
        x.onerror = () => reject(new Error('Upload failed — connection lost'))
        x.send(f)
      })
      msg.value = `Uploaded ${f.name}`
    } catch (e) { err.value = errorMessage(e); break } finally { upload.busy = false }
  }
  await load()
}

// ---- editor
const editor = reactive({ open: false, name: '', content: '', original: '', writable: false, busy: false, err: '' })
async function openEditor(name: string) {
  editor.err = ''
  try {
    const r = await api.get<{ content: string; writable: boolean }>(`/api/files/read?path=${encodeURIComponent(join(name))}`)
    Object.assign(editor, { open: true, name, content: r.content, original: r.content, writable: r.writable })
  } catch (e) { err.value = errorMessage(e) }
}
async function saveEditor() {
  editor.busy = true; editor.err = ''
  try { await api.post('/api/files/write', { path: join(editor.name), content: editor.content }); editor.original = editor.content; msg.value = `Saved ${editor.name}`; load() } catch (e) { editor.err = errorMessage(e) } finally { editor.busy = false }
}
function onKey(e: KeyboardEvent) {
  if ((e.metaKey || e.ctrlKey) && e.key === 's' && editor.open) { e.preventDefault(); if (editor.writable) saveEditor() }
  if (e.key === 'Tab' && (e.target as HTMLElement)?.tagName === 'TEXTAREA') {
    e.preventDefault()
    const t = e.target as HTMLTextAreaElement
    const s = t.selectionStart
    editor.content = editor.content.slice(0, s) + '    ' + editor.content.slice(t.selectionEnd)
    nextTick(() => { t.selectionStart = t.selectionEnd = s + 4 })
  }
}
function open(e: Entry) {
  if (e.kind === 'dir') go(join(e.name))
  else if (TEXT.test(e.name) && e.size <= 2 * 1024 * 1024) openEditor(e.name)
  else window.location.href = `/api/files/download?path=${encodeURIComponent(join(e.name))}`
}
</script>

<template>
  <header style="display: flex; flex-wrap: wrap; align-items: flex-end; justify-content: space-between; gap: 16px">
    <div style="display: flex; flex-direction: column; gap: 6px">
      <p class="lbl">{{ host }} / File manager</p>
      <h1 style="font-size: 30px; font-weight: 600">File manager</h1>
    </div>
    <div v-if="path && writable" style="display: flex; flex-wrap: wrap; gap: 8px">
      <button type="button" class="btn btn-s" @click="openBar('file')"><AppIcon name="file" :size="15" /> New file</button>
      <button type="button" class="btn btn-s" @click="openBar('folder')"><AppIcon name="folder" :size="15" /> New folder</button>
      <button type="button" class="btn btn-p" :disabled="upload.busy" @click="fileInput?.click()"><AppIcon name="upload" :size="15" /> Upload</button>
      <input ref="fileInput" type="file" multiple hidden @change="onPick">
    </div>
  </header>

  <!-- roots -->
  <section v-if="!path" aria-label="Folders" style="display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 12px">
    <button v-for="r in roots" :key="r.path" type="button" class="card" style="padding: 18px; display: flex; align-items: center; gap: 12px; cursor: pointer; text-align: left; border: 1px solid #222A34; color: inherit" @click="go(r.path)">
      <AppIcon name="folder" :size="22" />
      <span style="display: flex; flex-direction: column; gap: 2px"><span class="mono" style="font-size: 15px; font-weight: 600">{{ r.path }}</span><span style="font-size: 12px; color: #A3AEBD">{{ r.writable ? 'read and write' : 'read only' }}</span></span>
    </button>
    <p v-if="!roots.length && !loading" style="color: #A3AEBD">{{ err || 'No folders you can open.' }}</p>
  </section>

  <section v-else class="card" aria-label="Files" style="overflow: hidden">
    <div style="display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px; padding: 14px 20px">
      <nav aria-label="Path" class="mono" style="display: flex; flex-wrap: wrap; align-items: center; gap: 4px; font-size: 14px">
        <button type="button" class="btn btn-s" style="min-height: 30px; padding: 0 10px" @click="go('')"><AppIcon name="grid" :size="14" /></button>
        <template v-for="(c, i) in crumbs" :key="c.path">
          <span style="color: #5B6573">/</span>
          <button type="button" :style="`background: none; border: 0; cursor: pointer; padding: 2px 4px; color: ${i === crumbs.length - 1 ? '#E7ECF2' : '#A3AEBD'}; font-weight: ${i === crumbs.length - 1 ? 600 : 400}; font-family: inherit; font-size: inherit`" @click="go(c.path)">{{ c.name }}</button>
        </template>
        <span v-if="listing && !writable" class="pill" style="margin-left: 8px">read only</span>
      </nav>
      <input v-model="q" class="in" type="search" placeholder="Filter this folder" aria-label="Filter files" style="width: 220px">
    </div>

    <div v-if="selected.length && writable" style="display: flex; flex-wrap: wrap; gap: 8px; align-items: center; padding: 10px 20px; background: #0F1318; border-top: 1px solid #1B222B">
      <span style="font-size: 13px; color: #C5CEDA">{{ selected.length }} selected</span>
      <button type="button" class="btn btn-s" @click="openBar('compress', '', selected.length === 1 ? `${selected[0]}.zip` : 'archive.zip')"><AppIcon name="box" :size="14" /> Compress</button>
      <ConfirmAction label="Delete" :confirm-label="`Delete ${selected.length}`" question="Delete permanently?" @confirm="removeSelected" />
    </div>

    <form v-if="bar.mode" style="display: flex; flex-wrap: wrap; gap: 8px; align-items: flex-end; padding: 12px 20px; background: #0F1318; border-top: 1px solid #1B222B" @submit.prevent="submitBar">
      <label style="flex: 1 1 260px; display: flex; flex-direction: column; gap: 6px"><span style="font-size: 13px; font-weight: 500; color: #C5CEDA">{{ barLabel[bar.mode] }}<template v-if="bar.target"> — {{ bar.target }}</template></span><input v-model="bar.value" class="in mono" type="text" :placeholder="barPh[bar.mode]" spellcheck="false" autofocus></label>
      <label v-if="bar.mode === 'chmod'" style="display: flex; gap: 6px; align-items: center; font-size: 13px; color: #C5CEDA; min-height: 40px"><input v-model="bar.recursive" type="checkbox"> Include everything inside</label>
      <button type="submit" class="btn btn-p" :disabled="!bar.value.trim()">OK</button>
      <button type="button" class="btn btn-s" @click="bar.mode = ''">Cancel</button>
    </form>

    <div v-if="upload.busy" style="padding: 10px 20px; border-top: 1px solid #1B222B; display: flex; gap: 10px; align-items: center; font-size: 13px">
      <span class="mono">{{ upload.name }}</span>
      <div style="flex: 1; height: 6px; background: #1B222B; border-radius: 3px; overflow: hidden"><div :style="`width: ${upload.pct}%; height: 100%; background: #3DD68C`" /></div>
      <span class="mono">{{ upload.pct }}%</span>
    </div>
    <p v-if="err" class="small" style="color: var(--bad); padding: 8px 20px" role="alert">{{ err }}</p>
    <p v-else-if="msg" class="small" style="color: var(--ok); padding: 8px 20px" role="status">{{ msg }}</p>

    <div style="overflow-x: auto">
      <table>
        <thead><tr><th style="width: 32px"><input v-model="allChecked" type="checkbox" aria-label="Select all" :disabled="!writable"></th><th>Name</th><th>Size</th><th>Permissions</th><th>Owner</th><th>Modified</th><th><span style="position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0)">Actions</span></th></tr></thead>
        <tbody>
          <tr v-if="path !== '/' && crumbs.length > 1"><td /><td colspan="6"><button type="button" class="mono" style="background: none; border: 0; cursor: pointer; color: #A3AEBD; font-size: 13px" @click="go(crumbs[crumbs.length - 2]!.path)">.. (up)</button></td></tr>
          <tr v-for="e in shown" :key="e.name">
            <td><input v-model="selected" type="checkbox" :value="e.name" :aria-label="`Select ${e.name}`" :disabled="!writable"></td>
            <td>
              <button type="button" style="display: flex; align-items: center; gap: 8px; background: none; border: 0; cursor: pointer; color: #E7ECF2; font-size: 14px; padding: 0; text-align: left" @click="open(e)">
                <AppIcon :name="e.kind === 'dir' ? 'folder' : isArchive(e.name) ? 'box' : 'file'" :size="16" />
                <span class="mono">{{ e.name }}</span>
                <span v-if="e.link" class="mono" style="font-size: 12px; color: #A3AEBD">→ {{ e.link }}</span>
              </button>
            </td>
            <td class="mono" style="font-size: 13px">{{ e.kind === 'dir' ? '—' : formatBytes(e.size) }}</td>
            <td class="mono" style="font-size: 13px">{{ e.mode }}</td>
            <td class="mono" style="font-size: 13px; color: #C5CEDA">{{ e.owner }}</td>
            <td style="font-size: 13px; color: #C5CEDA">{{ fmtTime(e.modified) }}</td>
            <td>
              <div style="display: flex; gap: 6px">
                <a v-if="e.kind === 'file'" class="btn btn-s" :href="`/api/files/download?path=${encodeURIComponent(join(e.name))}`" :aria-label="`Download ${e.name}`"><AppIcon name="download" :size="13" /></a>
                <template v-if="writable">
                  <button v-if="isArchive(e.name)" type="button" class="btn btn-s" @click="openBar('extract', e.name, e.name.replace(/\.(zip|tar\.gz|tgz|tar\.bz2|tar\.xz|tar)$/i, ''))">Extract</button>
                  <button type="button" class="btn btn-s" :aria-label="`Rename ${e.name}`" @click="openBar('rename', e.name, e.name)"><AppIcon name="edit" :size="13" /></button>
                  <button type="button" class="btn btn-s mono" :aria-label="`Permissions of ${e.name}`" @click="openBar('chmod', e.name, e.mode)">{{ e.mode }}</button>
                </template>
              </div>
            </td>
          </tr>
          <tr v-if="!shown.length"><td colspan="7" style="color: #A3AEBD">{{ loading ? 'Loading…' : err ? '—' : 'This folder is empty.' }}</td></tr>
        </tbody>
      </table>
    </div>
  </section>

  <section v-if="editor.open" class="card" aria-labelledby="ed-h" style="padding: 0; overflow: hidden" @keydown="onKey">
    <div style="display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 10px; padding: 12px 20px; border-bottom: 1px solid #1B222B">
      <div style="display: flex; align-items: center; gap: 10px"><AppIcon name="edit" :size="16" /><h2 id="ed-h" class="mono" style="font-size: 15px; font-weight: 600">{{ editor.name }}</h2><span v-if="editor.content !== editor.original" class="pill warn">unsaved</span><span v-if="!editor.writable" class="pill">read only</span></div>
      <div style="display: flex; gap: 8px">
        <button v-if="editor.writable" type="button" class="btn btn-p" :disabled="editor.busy || editor.content === editor.original" @click="saveEditor"><AppIcon name="save" :size="14" /> {{ editor.busy ? 'Saving…' : 'Save' }}</button>
        <button type="button" class="btn btn-s" @click="editor.open = false">Close</button>
      </div>
    </div>
    <p v-if="editor.err" class="small" style="color: var(--bad); padding: 8px 20px" role="alert">{{ editor.err }}</p>
    <textarea v-model="editor.content" class="mono" :readonly="!editor.writable" spellcheck="false" aria-label="File contents" placeholder="Empty file — start typing" style="width: 100%; min-height: 480px; resize: vertical; background: #07090C; color: #E7ECF2; border: 0; padding: 16px 20px; font-size: 13px; line-height: 1.6; tab-size: 4; outline: none" />
    <p style="font-size: 12px; color: #A3AEBD; padding: 8px 20px">⌘/Ctrl + S saves. Owner and permissions are kept.</p>
  </section>
</template>
