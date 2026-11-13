<script setup lang="ts">
// Create a database (+ user with a generated password) and manage users.
const props = defineProps<{ engine: 'mysql' | 'postgres' }>()
const emit = defineEmits<{ changed: [] }>()
const api = useApi()

interface DbUser { user: string | null; host: string | null; databases: string | null }
const users = ref<DbUser[] | null>(null)
const err = ref<string | null>(null)
async function loadUsers() {
  try { users.value = await api.get<DbUser[]>(`/api/dbm/${props.engine}/users`) } catch (e) { err.value = errorMessage(e) }
}

const name = ref('')
const withUser = ref(true)
const busy = ref(false)
const created = ref<{ database: string; user: string | null; password: string | null; env: string | null } | null>(null)
// (watch placed after `created` is declared — immediate callbacks run synchronously)
watch(() => props.engine, () => { users.value = null; created.value = null; loadUsers() }, { immediate: true })
const okName = computed(() => /^[A-Za-z0-9_]{1,64}$/.test(name.value))
async function create() {
  busy.value = true
  err.value = null
  try {
    created.value = await api.post(`/api/dbm/${props.engine}/databases`, { name: name.value, user: withUser.value ? `${name.value}_user`.slice(0, 32) : null })
    name.value = ''
    emit('changed')
    loadUsers()
  } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) } finally { busy.value = false }
}
const newPw = ref<Record<string, string>>({})
async function reset(u: DbUser) {
  try {
    const r = await api.post<{ password: string }>(`/api/dbm/${props.engine}/users/password`, { user: u.user, host: u.host || 'localhost' })
    newPw.value = { ...newPw.value, [`${u.user}@${u.host}`]: r.password }
  } catch (e) { if (!(e instanceof SudoCancelled)) err.value = errorMessage(e) }
}
async function copy(t: string) { await navigator.clipboard?.writeText(t) }
const SYSTEM = ['root', 'postgres', 'debian-sys-maint', 'mysql.sys', 'mysql.session', 'mysql.infoschema']
</script>

<template>
  <div class="grid-two">
    <section class="card card-pad stack" style="gap: 12px" aria-labelledby="newdb-h">
      <h2 id="newdb-h">Create database</h2>
      <label class="stack" style="gap: 6px"><span class="small text-2">Name</span>
        <input v-model="name" class="in mono" type="text" placeholder="blog_prod" spellcheck="false"></label>
      <label class="small"><input v-model="withUser" type="checkbox"> Also create user <span class="mono">{{ (name || 'name') + '_user' }}</span> with a generated password</label>
      <div><button type="button" class="btn btn-p" :disabled="!okName || busy" @click="create"><span v-if="busy" class="spinner" aria-hidden="true" /> Create</button></div>
      <div v-if="created" class="card creds stack" style="gap: 8px">
        <span style="color: var(--ok)">Created <span class="mono">{{ created.database }}</span>.</span>
        <template v-if="created.env">
          <span class="small muted">Copy these into your project's .env now — the password is shown only once.</span>
          <pre class="mono">{{ created.env }}</pre>
          <button type="button" class="btn btn-s" style="align-self: flex-start" @click="copy(created.env!)">Copy</button>
        </template>
      </div>
    </section>

    <section class="card card-pad stack" style="gap: 10px" aria-labelledby="users-h">
      <h2 id="users-h">Users</h2>
      <p v-if="err" class="small" style="color: var(--bad)">{{ err }}</p>
      <div v-for="u in users ?? []" :key="`${u.user}@${u.host}`" class="urow">
        <div class="stack" style="gap: 2px; min-width: 0">
          <span class="mono">{{ u.user }}<span class="muted">@{{ u.host }}</span></span>
          <span class="small muted">{{ u.databases || 'no database grants' }}</span>
          <span v-if="newPw[`${u.user}@${u.host}`]" class="small" style="color: var(--ok)">New password: <span class="mono">{{ newPw[`${u.user}@${u.host}`] }}</span> <button type="button" class="link" @click="copy(newPw[`${u.user}@${u.host}`]!)">copy</button></span>
        </div>
        <ConfirmAction v-if="!SYSTEM.includes(u.user ?? '')" label="New password" :question="`Generate a new password for ${u.user}? Apps using the old one stop connecting until you update their .env.`" confirm-label="Generate" @confirm="reset(u)" />
      </div>
    </section>
  </div>
</template>

<style scoped>
.grid-two { display: grid; grid-template-columns: repeat(auto-fit, minmax(340px, 1fr)); gap: 16px; }
.in { min-height: 40px; padding: 0 10px; background: var(--sunken, #0F1318); border: 1px solid var(--btn-line, #2A3441); border-radius: 8px; color: var(--text); font-size: 14px; }
.creds { padding: 12px; background: var(--sunken, #0F1318); }
.creds pre { margin: 0; padding: 10px; background: #07090C; border-radius: 8px; font-size: 12px; white-space: pre-wrap; overflow-wrap: anywhere; }
.urow { display: flex; justify-content: space-between; align-items: center; gap: 12px; padding: 10px 0; border-bottom: 1px solid #1B222B; }
.link { background: none; border: 0; color: #7CC4FF; cursor: pointer; font-size: 12px; }
input[type=checkbox] { accent-color: var(--accent); }
</style>
