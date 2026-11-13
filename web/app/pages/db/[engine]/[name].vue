<script setup lang="ts">
// phpMyAdmin-style browser for one database: tables, rows, structure, SQL.
const route = useRoute()
const api = useApi()
const engine = computed(() => String(route.params.engine))
const db = computed(() => String(route.params.name))
useHead({ title: () => `${db.value} · Databases · NexaCloud` })

type Cell = string | null
interface TableInfo { name: string; rows: number; data_bytes: number; index_bytes: number; engine: string | null; type: string | null }
interface ColInfo { name: string; type: string; nullable: boolean; key: string | null; default: string | null; extra: string | null }
interface RowsResp { columns: string[]; rows: Cell[][]; total: number; page: number; per: number; primary_key: string[]; column_types: ColInfo[] }
interface QueryResp { columns: string[]; rows: Cell[][]; truncated: boolean; affected: number | null; elapsed_ms: number; read_only: boolean }

const tables = useResource<TableInfo[]>(`/api/dbm/${engine.value}/${encodeURIComponent(db.value)}/tables`)
const current = ref<string | null>(typeof route.query.t === 'string' ? route.query.t : null)
const tab = ref<'browse' | 'structure' | 'sql'>(current.value ? 'browse' : 'sql')
const tfilter = ref('')
const shownTables = computed(() => (tables.data.value ?? []).filter((t) => t.name.toLowerCase().includes(tfilter.value.toLowerCase())))

// ---------------------------------------------------------------- browse
const page = ref(1)
const per = ref(50)
const sort = ref<string | null>(null)
const dir = ref<'asc' | 'desc'>('asc')
const search = ref('')
const data = ref<RowsResp | null>(null)
const loading = ref(false)
const err = ref<string | null>(null)
const isMy = computed(() => engine.value === 'mysql')

async function loadRows() {
  if (!current.value) return
  loading.value = true
  err.value = null
  const qs = new URLSearchParams({ page: String(page.value), per: String(per.value) })
  if (sort.value) { qs.set('sort', sort.value); qs.set('dir', dir.value) }
  if (search.value.trim()) qs.set('q', search.value.trim())
  try {
    data.value = await api.get<RowsResp>(`/api/dbm/${engine.value}/${encodeURIComponent(db.value)}/tables/${encodeURIComponent(current.value)}/rows?${qs}`)
  } catch (e) { err.value = errorMessage(e) } finally { loading.value = false }
}
function open(t: string) {
  current.value = t; page.value = 1; sort.value = null; search.value = ''; editing.value = null
  if (tab.value === 'sql') tab.value = 'browse'
  navigateTo({ query: { t } }, { replace: true })
  loadRows(); loadStructure()
}
function sortBy(c: string) {
  if (sort.value === c) dir.value = dir.value === 'asc' ? 'desc' : 'asc'
  else { sort.value = c; dir.value = 'asc' }
  loadRows()
}
const pages = computed(() => (data.value ? Math.max(1, Math.ceil(data.value.total / data.value.per)) : 1))

// ---------------------------------------------------------------- structure
const structure = ref<{ columns: ColInfo[]; primary_key: string[]; indexes: Record<string, unknown>[] } | null>(null)
async function loadStructure() {
  if (!current.value) return
  try { structure.value = await api.get(`/api/dbm/${engine.value}/${encodeURIComponent(db.value)}/tables/${encodeURIComponent(current.value)}/structure`) } catch { structure.value = null }
}

// ---------------------------------------------------------------- row edit
const editing = ref<{ mode: 'update' | 'insert'; values: Record<string, string>; nulls: Record<string, boolean>; pk: Record<string, Cell> } | null>(null)
const rowMsg = ref<string | null>(null)
function startEdit(row: Cell[]) {
  const d = data.value!
  const values: Record<string, string> = {}
  const nulls: Record<string, boolean> = {}
  const pk: Record<string, Cell> = {}
  d.columns.forEach((c, i) => { values[c] = row[i] ?? ''; nulls[c] = row[i] === null; if (d.primary_key.includes(c)) pk[c] = row[i] ?? null })
  editing.value = { mode: 'update', values, nulls, pk }
}
function startInsert() {
  const d = data.value!
  editing.value = { mode: 'insert', values: Object.fromEntries(d.columns.map((c) => [c, ''])), nulls: Object.fromEntries(d.columns.map((c) => [c, false])), pk: {} }
}
async function saveRow() {
  const e = editing.value!
  const set: Record<string, string | null> = {}
  for (const c of Object.keys(e.values)) {
    if (e.mode === 'insert' && !e.values[c] && !e.nulls[c]) continue
    set[c] = e.nulls[c] ? null : e.values[c]!
  }
  try {
    const r = await api.post<{ affected: number | null }>(`/api/dbm/mysql/${encodeURIComponent(db.value)}/tables/${encodeURIComponent(current.value!)}/rows/${e.mode}`, { pk: e.pk, set })
    rowMsg.value = `${e.mode === 'insert' ? 'Inserted' : 'Saved'} (${r.affected ?? 0} row affected).`
    editing.value = null
    loadRows()
  } catch (x) { if (!(x instanceof SudoCancelled)) rowMsg.value = errorMessage(x) }
}
async function deleteRow(row: Cell[]) {
  const d = data.value!
  const pk = Object.fromEntries(d.primary_key.map((c) => [c, row[d.columns.indexOf(c)] ?? null]))
  try {
    await api.post(`/api/dbm/mysql/${encodeURIComponent(db.value)}/tables/${encodeURIComponent(current.value!)}/rows/delete`, { pk })
    rowMsg.value = 'Row deleted.'
    loadRows()
  } catch (x) { if (!(x instanceof SudoCancelled)) rowMsg.value = errorMessage(x) }
}

// ---------------------------------------------------------------- SQL
const sql = ref(current.value ? `SELECT * FROM ${isMy.value ? '`' + current.value + '`' : '"' + current.value + '"'} LIMIT 50;` : 'SHOW TABLES;')
if (!isMy.value && !current.value) sql.value = "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public';"
const writeMode = ref(false)
const confirmText = ref('')
const needConfirm = ref(false)
const qres = ref<QueryResp | null>(null)
const qerr = ref<string | null>(null)
const running = ref(false)
async function run() {
  running.value = true
  qerr.value = null
  try {
    const path = `/api/dbm/${engine.value}/${encodeURIComponent(db.value)}/query${writeMode.value ? '/write' : ''}`
    qres.value = await api.post<QueryResp>(path, { sql: sql.value, confirm: needConfirm.value ? confirmText.value : undefined })
    needConfirm.value = false
    confirmText.value = ''
    if (writeMode.value) tables.refresh()
  } catch (e) {
    const m = errorMessage(e)
    if (m.startsWith('confirm_required')) { needConfirm.value = true; qerr.value = m.replace('confirm_required: ', '') }
    else if (!(e instanceof SudoCancelled)) qerr.value = m
  } finally { running.value = false }
}
function onKey(e: KeyboardEvent) { if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') { e.preventDefault(); run() } }

onMounted(() => { if (current.value) { loadRows(); loadStructure() } })
</script>

<template>
  <PageHeader :title="db" :crumb="`${engine === 'mysql' ? 'MySQL' : 'PostgreSQL'} database`">
    <NuxtLink to="/databases" class="btn btn-s">All databases</NuxtLink>
  </PageHeader>

  <div class="dbm">
    <aside class="card tables" aria-label="Tables">
      <input v-model="tfilter" class="in" type="search" placeholder="Filter tables" aria-label="Filter tables">
      <div v-if="tables.loading.value" style="padding: 8px"><SkeletonRows :rows="6" /></div>
      <ErrorBox v-else-if="tables.error.value" :message="tables.error.value" @retry="tables.refresh()" />
      <ul v-else>
        <li v-for="t in shownTables" :key="t.name">
          <button type="button" :class="{ on: current === t.name }" @click="open(t.name)">
            <span class="mono">{{ t.name }}</span>
            <span class="small muted">{{ t.type === 'VIEW' ? 'view' : `~${t.rows}` }}</span>
          </button>
        </li>
        <li v-if="!shownTables.length" class="small muted" style="padding: 8px">No tables</li>
      </ul>
    </aside>

    <section class="main-col stack" style="gap: 14px; min-width: 0">
      <div class="seg" role="tablist">
        <button type="button" role="tab" :aria-selected="tab === 'browse'" :class="{ on: tab === 'browse' }" :disabled="!current" @click="tab = 'browse'">Browse</button>
        <button type="button" role="tab" :aria-selected="tab === 'structure'" :class="{ on: tab === 'structure' }" :disabled="!current" @click="tab = 'structure'">Structure</button>
        <button type="button" role="tab" :aria-selected="tab === 'sql'" :class="{ on: tab === 'sql' }" @click="tab = 'sql'">SQL</button>
      </div>

      <!-- browse -->
      <template v-if="tab === 'browse' && current">
        <div class="row wrap" style="gap: 8px">
          <strong class="mono">{{ current }}</strong>
          <span v-if="data" class="small muted">{{ data.total.toLocaleString() }} rows</span>
          <input v-model="search" class="in" type="search" placeholder="Search all columns" style="max-width: 260px; margin-left: auto" @keyup.enter="page = 1; loadRows()">
          <button type="button" class="btn btn-s" @click="page = 1; loadRows()">Search</button>
          <button v-if="isMy && data" type="button" class="btn btn-s" @click="startInsert"><AppIcon name="plus" :size="14" :stroke="2" /> Insert row</button>
        </div>
        <p v-if="rowMsg" class="small" :style="{ color: rowMsg.includes('ed') ? 'var(--ok)' : 'var(--bad)' }">{{ rowMsg }}</p>

        <div v-if="editing" class="card edit-box stack" style="gap: 10px">
          <strong>{{ editing.mode === 'insert' ? 'Insert row' : 'Edit row' }}</strong>
          <div class="edit-grid">
            <label v-for="c in data?.column_types ?? []" :key="c.name" class="stack" style="gap: 4px">
              <span class="small"><span class="mono">{{ c.name }}</span> <span class="muted">{{ c.type }}{{ c.key === 'PRI' ? ' · primary key' : '' }}</span></span>
              <textarea v-model="editing.values[c.name]" class="in mono" rows="1" :disabled="editing.nulls[c.name]" />
              <label v-if="c.nullable" class="small muted"><input v-model="editing.nulls[c.name]" type="checkbox"> NULL</label>
            </label>
          </div>
          <div class="row" style="gap: 8px"><button type="button" class="btn btn-s btn-p" @click="saveRow">Save</button><button type="button" class="btn btn-s btn-ghost" @click="editing = null">Cancel</button><span class="small muted">Needs your password (sudo) · recorded in the audit log</span></div>
        </div>

        <p v-if="err" class="small" style="color: var(--bad)">{{ err }}</p>
        <div v-if="data" class="card grid-wrap">
          <table class="grid">
            <thead><tr>
              <th v-if="isMy && data.primary_key.length" style="width: 1%" />
              <th v-for="c in data.columns" :key="c"><button type="button" class="th-btn" @click="sortBy(c)">{{ c }}<span v-if="sort === c">{{ dir === 'asc' ? ' ▲' : ' ▼' }}</span></button></th>
            </tr></thead>
            <tbody>
              <tr v-for="(r, i) in data.rows" :key="i">
                <td v-if="isMy && data.primary_key.length" class="acts">
                  <button type="button" class="link" @click="startEdit(r)">Edit</button>
                  <ConfirmAction label="Delete" question="Delete this row?" confirm-label="Delete" @confirm="deleteRow(r)" />
                </td>
                <td v-for="(v, j) in r" :key="j" :class="{ nullv: v === null }" :title="v ?? 'NULL'">{{ v === null ? 'NULL' : v.length > 120 ? v.slice(0, 120) + '…' : v }}</td>
              </tr>
              <tr v-if="!data.rows.length"><td :colspan="data.columns.length + 1" class="muted" style="padding: 20px">No rows</td></tr>
            </tbody>
          </table>
        </div>
        <div v-if="data" class="row wrap" style="gap: 8px">
          <button type="button" class="btn btn-s" :disabled="page <= 1 || loading" @click="page--; loadRows()">Previous</button>
          <span class="small">Page {{ page }} / {{ pages }}</span>
          <button type="button" class="btn btn-s" :disabled="page >= pages || loading" @click="page++; loadRows()">Next</button>
          <select v-model.number="per" class="in" style="width: auto" @change="page = 1; loadRows()"><option :value="25">25</option><option :value="50">50</option><option :value="100">100</option><option :value="250">250</option></select>
          <span v-if="loading" class="spinner" aria-hidden="true" />
        </div>
      </template>

      <!-- structure -->
      <template v-if="tab === 'structure' && structure">
        <div class="card grid-wrap">
          <table class="grid">
            <thead><tr><th>Column</th><th>Type</th><th>Null</th><th>Key</th><th>Default</th><th>Extra</th></tr></thead>
            <tbody><tr v-for="c in structure.columns" :key="c.name">
              <td class="mono">{{ c.name }}</td><td class="mono">{{ c.type }}</td><td>{{ c.nullable ? 'yes' : 'no' }}</td>
              <td>{{ c.key }}</td><td class="mono">{{ c.default ?? '—' }}</td><td>{{ c.extra }}</td>
            </tr></tbody>
          </table>
        </div>
        <div v-if="structure.indexes.length" class="card card-pad stack" style="gap: 6px">
          <strong>Indexes</strong>
          <span v-for="(ix, i) in structure.indexes" :key="i" class="mono small">{{ ix.name }} {{ ix.column ?? ix.definition }} {{ ix.unique ? '(unique)' : '' }}</span>
        </div>
      </template>

      <!-- sql -->
      <template v-if="tab === 'sql'">
        <textarea v-model="sql" class="sql mono" rows="7" spellcheck="false" aria-label="SQL query" @keydown="onKey" />
        <div class="row wrap" style="gap: 10px">
          <button type="button" class="btn btn-p" :disabled="running" @click="run"><span v-if="running" class="spinner" aria-hidden="true" /> Run <span class="small" style="opacity: .7">⌘↵</span></button>
          <label class="small row" style="gap: 6px"><input v-model="writeMode" type="checkbox"> Allow changes (INSERT/UPDATE/DELETE/DDL — asks for your password)</label>
          <span class="small muted">{{ writeMode ? 'Write mode' : 'Read-only: the database refuses any change' }} · 30 s limit · max 1000 rows</span>
        </div>
        <div v-if="needConfirm" class="card edit-box row wrap" style="gap: 8px; border-color: var(--warn)">
          <span class="small" style="color: var(--warn)">Type <span class="mono">{{ db }}</span> to run this destructive statement:</span>
          <input v-model="confirmText" class="in mono" type="text" style="max-width: 220px">
          <button type="button" class="btn btn-s" :disabled="confirmText !== db" @click="run">Run anyway</button>
        </div>
        <p v-if="qerr" class="small" style="color: var(--bad); white-space: pre-wrap" role="alert">{{ qerr }}</p>
        <template v-if="qres">
          <span class="small muted">{{ qres.rows.length }} row{{ qres.rows.length === 1 ? '' : 's' }}<template v-if="qres.truncated"> (first 1000)</template><template v-if="qres.affected !== null && qres.affected >= 0"> · {{ qres.affected }} affected</template> · {{ qres.elapsed_ms }} ms</span>
          <div v-if="qres.columns.length" class="card grid-wrap">
            <table class="grid">
              <thead><tr><th v-for="c in qres.columns" :key="c">{{ c }}</th></tr></thead>
              <tbody><tr v-for="(r, i) in qres.rows" :key="i"><td v-for="(v, j) in r" :key="j" :class="{ nullv: v === null }">{{ v === null ? 'NULL' : v }}</td></tr></tbody>
            </table>
          </div>
        </template>
      </template>
    </section>
  </div>
</template>

<style scoped>
.dbm { display: flex; flex-wrap: wrap; gap: 16px; align-items: flex-start; }
.tables { flex: 1 1 220px; max-width: 280px; padding: 10px; display: flex; flex-direction: column; gap: 8px; max-height: 75vh; overflow: auto; }
.tables ul { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 2px; }
.tables button { width: 100%; display: flex; justify-content: space-between; gap: 8px; min-height: 36px; padding: 0 10px; border: 0; border-radius: 7px; background: transparent; color: var(--text-2); cursor: pointer; text-align: left; font-size: 13px; }
.tables button:hover { background: #171D25; }
.tables button.on { background: #1B2430; color: var(--text); }
.main-col { flex: 999 1 500px; }
.seg { display: flex; gap: 4px; padding: 4px; background: var(--card, #12171D); border: 1px solid var(--line, #222A34); border-radius: 10px; align-self: flex-start; }
.seg button { min-height: 36px; padding: 0 16px; border-radius: 7px; border: 0; background: transparent; color: var(--text-2); font-weight: 600; cursor: pointer; }
.seg button.on { background: #1F2833; color: var(--text); }
.seg button:disabled { opacity: .4; cursor: default; }
.in { min-height: 36px; padding: 0 10px; background: var(--sunken, #0F1318); border: 1px solid var(--btn-line, #2A3441); border-radius: 8px; color: var(--text); font-size: 13px; box-sizing: border-box; }
.grid-wrap { overflow: auto; max-height: 65vh; }
.grid { border-collapse: collapse; width: max-content; min-width: 100%; font-size: 12.5px; }
.grid th, .grid td { padding: 7px 12px; border-bottom: 1px solid #1B222B; text-align: left; white-space: nowrap; max-width: 380px; overflow: hidden; text-overflow: ellipsis; }
.grid th { position: sticky; top: 0; background: var(--card, #12171D); font-weight: 600; color: var(--text-2); }
.grid td { font-family: var(--font-mono, ui-monospace); }
.th-btn { background: none; border: 0; color: inherit; font: inherit; cursor: pointer; padding: 0; }
.nullv { color: #6F7B8C; font-style: italic; }
.acts { display: flex; gap: 8px; align-items: center; }
.link { background: none; border: 0; color: #7CC4FF; cursor: pointer; padding: 0; font-size: 12px; }
.edit-box { padding: 14px; background: var(--sunken, #0F1318); }
.edit-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); gap: 10px; }
.edit-grid textarea { min-height: 36px; padding: 8px 10px; resize: vertical; }
.sql { width: 100%; box-sizing: border-box; background: #07090C; color: var(--text); border: 1px solid #1B222B; border-radius: 10px; padding: 14px; font-size: 13px; line-height: 1.6; resize: vertical; }
input[type=checkbox] { accent-color: var(--accent); }
</style>
