<script setup lang="ts">
import type { Site } from '~/types/api'

useHead({ title: 'Sites · NexaCloud' })

const route = useRoute()
const sites = useResource<Site[]>('/api/sites')

const initialQ = typeof route.query.q === 'string' ? route.query.q : ''
const query = ref(initialQ)
const expanded = ref<Set<string>>(new Set(initialQ ? [initialQ] : []))

const filtered = computed(() => {
  const list = sites.data.value ?? []
  const q = query.value.trim().toLowerCase()
  if (!q) return list
  return list.filter((s) => {
    const hay = [
      ...s.server_names,
      s.id,
      s.root ?? '',
      s.kind,
      siteKindLabel(s.kind),
      s.php_version ? `php ${s.php_version}` : '',
      s.proxy_pass ?? '',
      s.git?.branch ?? '',
      s.git?.remote ?? '',
    ].join(' ').toLowerCase()
    return hay.includes(q)
  })
})

function toggle(id: string) {
  const next = new Set(expanded.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  expanded.value = next
}

function rowId(id: string) {
  return `site-${id.replace(/[^A-Za-z0-9_-]/g, '_')}`
}
</script>

<template>
  <PageHeader title="Sites" crumb="Nginx sites on this server">
    <label class="search">
      <AppIcon name="search" :size="16" :stroke="2" class="muted" />
      <span class="sr-only">Search sites</span>
      <input v-model="query" type="search" placeholder="Search domains, paths, PHP, branch">
    </label>
  </PageHeader>

  <InfoNote>
    NexaCloud reads your existing Nginx config (read-only). Creating and editing sites arrives in v0.2.
  </InfoNote>

  <section class="card" aria-labelledby="sites-h" style="overflow: hidden">
    <div class="card-head">
      <h2 id="sites-h">
        All sites
        <span v-if="sites.data.value" class="muted" style="font-weight: 500">· {{ sites.data.value.length }}</span>
      </h2>
      <button type="button" class="btn btn-s" :disabled="sites.refreshing.value" @click="sites.refresh()">
        <span v-if="sites.refreshing.value" class="spinner" aria-hidden="true" />
        <AppIcon v-else name="restart" :size="14" :stroke="2" />
        Rescan
      </button>
    </div>

    <div v-if="sites.loading.value" style="padding: 0 20px 20px"><SkeletonRows :rows="6" /></div>
    <div v-else-if="sites.error.value && !sites.data.value" style="padding: 0 20px 20px">
      <ErrorBox :message="sites.error.value" :retrying="sites.refreshing.value" @retry="sites.refresh()" />
    </div>
    <div v-else-if="!sites.data.value?.length" class="empty">
      <strong>No sites found</strong>
      <span>Nothing in <span class="mono">/etc/nginx/sites-enabled/</span> or <span class="mono">/etc/nginx/conf.d/*.conf</span>.</span>
    </div>
    <div v-else-if="!filtered.length" class="empty">
      <strong>No matches</strong>
      <span>No site matches “{{ query }}”.</span>
      <button type="button" class="btn btn-s" @click="query = ''">Clear search</button>
    </div>
    <div v-else class="table-wrap">
      <table>
        <thead>
          <tr>
            <th style="width: 1%"><span class="sr-only">Details</span></th>
            <th>Site</th>
            <th>Type</th>
            <th>PHP</th>
            <th>SSL</th>
            <th>Root</th>
            <th>Git</th>
          </tr>
        </thead>
        <tbody>
          <template v-for="s in filtered" :key="s.id">
            <tr class="site-row" :class="{ open: expanded.has(s.id) }" @click="toggle(s.id)">
              <td style="padding-right: 0">
                <button
                  type="button"
                  class="icon-btn chev"
                  :aria-expanded="expanded.has(s.id)"
                  :aria-controls="rowId(s.id)"
                  :aria-label="`${expanded.has(s.id) ? 'Hide' : 'Show'} details for ${s.server_names[0] ?? s.id}`"
                  @click.stop="toggle(s.id)"
                >
                  <AppIcon name="chevron" :size="16" :stroke="2" />
                </button>
              </td>
              <td>
                <div class="cell-stack">
                  <span style="font-weight: 600">{{ s.server_names[0] ?? s.id }}</span>
                  <span v-if="s.server_names.length > 1" class="cell-sub">
                    +{{ s.server_names.length - 1 }} more {{ s.server_names.length === 2 ? 'name' : 'names' }}
                  </span>
                </div>
              </td>
              <td><span class="tag">{{ siteKindLabel(s.kind) }}</span></td>
              <td>
                <span v-if="s.php_version" class="tag mono">PHP {{ s.php_version }}</span>
                <span v-else class="muted">—</span>
              </td>
              <td>
                <div class="cell-stack">
                  <StatusPill v-bind="sslStatus(s)" style="align-self: flex-start" />
                  <span v-if="s.ssl && s.ssl_expires" class="cell-sub">until {{ formatDate(s.ssl_expires) }}</span>
                </div>
              </td>
              <td class="mono" style="font-size: 13px">
                <template v-if="s.root">{{ s.root }}</template>
                <template v-else-if="s.proxy_pass">→ {{ s.proxy_pass }}</template>
                <span v-else class="muted">—</span>
              </td>
              <td>
                <div v-if="s.git" class="cell-stack">
                  <span class="mono" style="font-size: 13px">{{ [s.git.branch, s.git.commit].filter(Boolean).join('@') || '—' }}</span>
                  <span v-if="s.git.message" class="cell-sub truncate">{{ s.git.message }}</span>
                </div>
                <span v-else class="muted">—</span>
              </td>
            </tr>
            <tr v-if="expanded.has(s.id)" :id="rowId(s.id)" class="detail-row">
              <td colspan="7">
                <dl class="details">
                  <div class="detail detail-wide">
                    <dt class="lbl">Server names</dt>
                    <dd class="names">
                      <span v-for="n in s.server_names" :key="n" class="tag mono">{{ n }}</span>
                      <span v-if="!s.server_names.length" class="muted">none</span>
                    </dd>
                  </div>
                  <div class="detail">
                    <dt class="lbl">Config file</dt>
                    <dd class="mono">{{ s.config_path }}</dd>
                  </div>
                  <div class="detail">
                    <dt class="lbl">Document root</dt>
                    <dd class="mono">{{ s.root ?? '—' }}</dd>
                  </div>
                  <div v-if="s.proxy_pass" class="detail">
                    <dt class="lbl">Proxies to</dt>
                    <dd class="mono">{{ s.proxy_pass }}</dd>
                  </div>
                  <div class="detail">
                    <dt class="lbl">SSL certificate</dt>
                    <dd>
                      <template v-if="!s.ssl">Not configured</template>
                      <template v-else-if="s.ssl_expires">Expires {{ formatDateTime(s.ssl_expires) }} ({{ formatRelative(s.ssl_expires) }})</template>
                      <template v-else>Configured, expiry could not be read</template>
                    </dd>
                  </div>
                  <div class="detail">
                    <dt class="lbl">Git remote</dt>
                    <dd class="mono">{{ s.git?.remote ?? (s.git ? 'No origin remote' : 'Not a git checkout') }}</dd>
                  </div>
                  <div v-if="s.git?.commit" class="detail">
                    <dt class="lbl">Last commit</dt>
                    <dd><span class="mono">{{ s.git.commit }}</span><template v-if="s.git.message"> · {{ s.git.message }}</template></dd>
                  </div>
                </dl>
              </td>
            </tr>
          </template>
        </tbody>
      </table>
    </div>
  </section>
</template>

<style scoped>
.site-row { cursor: pointer; }
.site-row:hover td { background: #141A21; }
.site-row.open td { border-bottom-color: transparent; background: #141A21; }
.chev svg { transition: transform .15s ease; }
.site-row.open .chev svg { transform: rotate(90deg); }
.detail-row td { background: #141A21; padding: 4px 20px 20px 68px; white-space: normal; }
.details { margin: 0; display: grid; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr)); gap: 14px 24px; }
.detail { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
.detail-wide { grid-column: 1 / -1; }
.detail dd { margin: 0; font-size: 13px; color: var(--text-2); overflow-wrap: anywhere; }
.names { display: flex; flex-wrap: wrap; gap: 6px; }
.truncate { max-width: 240px; overflow: hidden; text-overflow: ellipsis; }
@media (max-width: 700px) { .detail-row td { padding-left: 16px; } }
</style>
