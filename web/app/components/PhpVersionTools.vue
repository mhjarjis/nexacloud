<script setup lang="ts">
// Per-version tools on the PHP page: CLI default, extensions, php.ini.
import type { PhpConfig } from '~/types/api'

const props = defineProps<{ version: string; isCli: boolean }>()
const emit = defineEmits<{ changed: [] }>()
const api = useApi()

const open = ref<'' | 'ext' | 'ini'>('')
const cfg = ref<PhpConfig | null>(null)
const err = ref<string | null>(null)
const picked = ref<string[]>([])
const ini = ref<Record<string, string>>({})

async function load() {
  try {
    cfg.value = await api.get<PhpConfig>(`/api/php/${props.version}/config`)
    ini.value = Object.fromEntries(cfg.value.ini_keys.map((k) => [k.key, cfg.value!.ini[k.key] ?? '']))
  } catch (e) { err.value = errorMessage(e) }
}
async function toggle(t: 'ext' | 'ini') {
  open.value = open.value === t ? '' : t
  if (open.value && !cfg.value) await load()
}
const missing = computed(() => (cfg.value?.optional_extensions ?? []).filter((e) => !cfg.value!.loaded_modules.includes(e)))
const loaded = computed(() => (cfg.value?.optional_extensions ?? []).filter((e) => cfg.value!.loaded_modules.includes(e)))
const iniChanges = computed(() => {
  const out: Record<string, string> = {}
  for (const [k, v] of Object.entries(ini.value)) if ((cfg.value?.ini[k] ?? '') !== v) out[k] = v
  return out
})
const HINTS: Record<string, string> = {
  memory_limit: 'e.g. 256M', upload_max_filesize: 'e.g. 64M', post_max_size: 'e.g. 64M', max_execution_time: 'seconds',
  'date.timezone': 'e.g. Europe/Amsterdam', 'opcache.memory_consumption': 'MB, e.g. 256',
}
function done(s: string) { if (s === 'applied') { cfg.value = null; picked.value = []; emit('changed'); if (open.value) load() } }
</script>

<template>
  <div class="stack" style="gap: 10px; padding-top: 10px; border-top: 1px solid var(--row-line)">
    <div class="row wrap" style="gap: 6px">
      <button type="button" class="btn btn-s" @click="toggle('ext')">Extensions</button>
      <button type="button" class="btn btn-s" @click="toggle('ini')">php.ini</button>
    </div>
    <PlanAction
      v-if="!isCli"
      :request="{ kind: 'php_cli', params: { version } }"
      label="Make CLI default"
      :success-text="`php now runs PHP ${version}.`"
      @done="done"
    />
    <p v-if="err" class="small" style="color: var(--bad)">{{ err }}</p>

    <div v-if="open === 'ext' && cfg" class="stack" style="gap: 8px">
      <span class="lbl">Installed</span>
      <div class="row wrap" style="gap: 6px"><span v-for="e in loaded" :key="e" class="tag mono">{{ e }}</span></div>
      <span class="lbl" style="margin-top: 6px">Add</span>
      <div class="ext-grid">
        <label v-for="e in missing" :key="e" class="small"><input v-model="picked" type="checkbox" :value="e"> <span class="mono">{{ e }}</span></label>
      </div>
      <PlanAction
        :key="picked.join(',')"
        :request="{ kind: 'php_extensions', params: { version, extensions: picked } }"
        label="Preview install"
        :disabled="!picked.length"
        success-text="Extensions installed and PHP-FPM reloaded."
        @done="done"
      />
    </div>

    <div v-if="open === 'ini' && cfg" class="stack" style="gap: 8px">
      <p class="small muted">Saved to <span class="mono">/etc/php/{{ version }}/fpm|cli/conf.d/99-nexacloud.ini</span>; validated with <span class="mono">php-fpm{{ version }} -t</span> before reload. Leave empty to use the default.</p>
      <div class="ini-grid">
        <label v-for="k in cfg.ini_keys" :key="k.key" class="stack" style="gap: 4px">
          <span class="mono small">{{ k.key }}</span>
          <input v-model="ini[k.key]" class="in mono" type="text" :placeholder="HINTS[k.key] ?? (k.type === 'bool' ? '0 or 1' : '')">
        </label>
      </div>
      <PlanAction
        :key="JSON.stringify(iniChanges)"
        :request="{ kind: 'php_ini', params: { version, settings: iniChanges } }"
        label="Preview"
        :disabled="!Object.keys(iniChanges).length"
        success-text="Settings saved and PHP-FPM reloaded."
        @done="done"
      />
    </div>
  </div>
</template>

<style scoped>
.ext-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(120px, 1fr)); gap: 6px; }
.ext-grid input { accent-color: var(--accent); }
.ini-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); gap: 10px; }
.in { min-height: 36px; padding: 0 10px; background: var(--sunken, #0F1318); border: 1px solid var(--btn-line, #2A3441); border-radius: 8px; color: var(--text); font-size: 13px; width: 100%; box-sizing: border-box; }
</style>
