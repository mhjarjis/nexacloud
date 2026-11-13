<script setup lang="ts">
import type { PhpInfo, PhpVersion } from '~/types/api'

useHead({ title: 'PHP · NexaCloud' })

const php = useResource<PhpInfo>('/api/php')

function versionKey(v: string): number {
  const [maj = 0, min = 0] = v.split('.').map((n) => Number.parseInt(n, 10) || 0)
  return maj * 1000 + min
}

const versions = computed<PhpVersion[]>(() =>
  [...(php.data.value?.versions ?? [])].sort((a, b) => versionKey(b.version) - versionKey(a.version)),
)
const cli = computed(() => php.data.value?.cli_default ?? null)
const ALL = ['8.5', '8.4', '8.3', '8.2', '8.1', '8.0', '7.4']
const notInstalled = computed(() => ALL.filter((v) => !(php.data.value?.versions ?? []).some((x) => x.version === v && x.fpm_installed)))
const installV = ref('8.4')
watch(notInstalled, (n) => { if (!n.includes(installV.value)) installV.value = n[0] ?? '' }, { immediate: true })
</script>

<template>
  <PageHeader title="PHP" crumb="Installed PHP versions">
    <span v-if="php.data.value" class="small muted">
      CLI default:
      <span class="mono" style="color: var(--text)">{{ cli ? `php${cli}` : 'not set' }}</span>
    </span>
  </PageHeader>

  <section class="card card-pad stack" style="gap: 10px" aria-label="Install PHP">
    <div class="row wrap" style="gap: 10px; align-items: center">
      <strong>Install a PHP version</strong>
      <select v-model="installV" class="in-sel" aria-label="PHP version to install">
        <option v-for="v in notInstalled" :key="v" :value="v">PHP {{ v }}</option>
      </select>
      <span class="small muted">From the ondrej/php PPA, with FPM and Laravel's extensions. Existing sites are not touched; switch a site from the Sites page.</span>
    </div>
    <PlanAction
      :key="installV"
      :request="{ kind: 'php_install', params: { version: installV } }"
      label="Preview install"
      :disabled="!installV"
      :success-text="`PHP ${installV} is installed.`"
      @done="php.refresh()"
    />
  </section>

  <div v-if="php.loading.value" class="grid-fill">
    <div v-for="i in 3" :key="i" class="card card-pad"><SkeletonRows :rows="4" label="Loading PHP versions" /></div>
  </div>
  <ErrorBox
    v-else-if="php.error.value && !php.data.value"
    :message="php.error.value"
    :retrying="php.refreshing.value"
    @retry="php.refresh()"
  />
  <div v-else-if="!versions.length" class="card empty">
    <strong>No PHP installed</strong>
    <span>No versions were found under <span class="mono">/etc/php/</span>.</span>
  </div>
  <section v-else aria-label="PHP versions" class="grid-fill">
    <article
      v-for="v in versions"
      :key="v.version"
      class="card php-card"
      :class="{ cli: v.version === cli }"
      :aria-label="`PHP ${v.version}`"
    >
      <div class="row" style="justify-content: space-between; gap: 8px">
        <span class="php-ver">{{ v.version }}</span>
        <StatusPill v-bind="phpSupport(v)" />
      </div>
      <span class="text-2" style="font-size: 13px">{{ phpSupport(v).detail }}</span>
      <div class="row wrap" style="gap: 6px">
        <StatusPill v-bind="fpmStatus(v)" />
        <span v-if="v.version === cli" class="tag">CLI default</span>
      </div>
      <div class="used-by">
        <span class="lbl">{{ v.sites.length ? `Used by ${v.sites.length} ${v.sites.length === 1 ? 'site' : 'sites'}` : 'Not used by any site' }}</span>
        <ul v-if="v.sites.length" class="site-list">
          <li v-for="s in v.sites" :key="s">
            <NuxtLink :to="{ path: '/sites', query: { q: s } }" class="mono">{{ s }}</NuxtLink>
          </li>
        </ul>
      </div>
      <PhpVersionTools v-if="v.fpm_installed" :version="v.version" :is-cli="v.version === cli" @changed="php.refresh()" />
    </article>
  </section>
</template>

<style scoped>
.php-card { padding: 16px; display: flex; flex-direction: column; gap: 12px; }
.php-card.cli { border-color: var(--accent); }
.php-ver { font-family: var(--font-head); font-size: 28px; font-weight: 600; line-height: 1; }
.used-by { display: flex; flex-direction: column; gap: 6px; padding-top: 10px; border-top: 1px solid var(--row-line); }
.site-list { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 4px; font-size: 13px; }
.site-list a { overflow-wrap: anywhere; }
.in-sel { min-height: 40px; padding: 0 10px; background: var(--sunken, #0F1318); border: 1px solid var(--btn-line, #2A3441); border-radius: 8px; color: var(--text); }
</style>
