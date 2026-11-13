// Follow a job's log live over SSE (`GET /api/jobs/{id}/stream`): the agent
// sends the existing log, then new lines, then one final `status` event.
import type { JobStatus } from '~/types/api'

export function useJobStream(jobId: Ref<string | null | undefined>) {
  const lines = ref<string[]>([])
  const status = ref<JobStatus | null>(null)
  const error = ref<string | null>(null)
  const connected = ref(false)
  let es: EventSource | null = null

  function close() {
    es?.close()
    es = null
    connected.value = false
  }

  function open(id: string) {
    close()
    lines.value = []
    status.value = null
    error.value = null
    if (typeof EventSource === 'undefined') return
    es = new EventSource(`/api/jobs/${encodeURIComponent(id)}/stream`, { withCredentials: true })
    // The server replays the whole log on each connection.
    es.onopen = () => { connected.value = true; lines.value = [] }
    es.addEventListener('log', (ev) => {
      lines.value.push((ev as MessageEvent<string>).data)
    })
    es.addEventListener('status', (ev) => {
      try {
        const d = JSON.parse((ev as MessageEvent<string>).data) as { status: JobStatus; error: string | null }
        status.value = d.status
        error.value = d.error
      } catch { /* ignore */ }
      close()
    })
    es.onerror = () => {
      // Finished streams are closed by us; anything else: stop, the caller
      // still has the polled job state.
      if (status.value == null) close()
    }
  }

  watch(jobId, (id) => (id ? open(id) : close()), { immediate: true })
  onBeforeUnmount(close)

  const text = computed(() => lines.value.join('\n'))
  return { lines, text, status, error, connected, close }
}
