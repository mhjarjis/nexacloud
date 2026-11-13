// Poll background jobs (`GET /api/jobs/{id}`) until they finish.
import type { Job } from '~/types/api'

export interface WatchedJob {
  id: string
  job: Job | null
  /** Set when polling stopped without a final status (e.g. agent restarted). */
  lost: string | null
}

export function useJobWatcher(opts: { interval?: number; onFinish?: (job: Job) => void } = {}) {
  const api = useApi()
  const interval = opts.interval ?? 1500
  const jobs = ref<Record<string, WatchedJob>>({})
  const timers = new Map<string, ReturnType<typeof setTimeout>>()
  let alive = true

  async function poll(id: string, failures = 0) {
    timers.delete(id)
    if (!alive) return
    try {
      const job = await api.get<Job>(`/api/jobs/${encodeURIComponent(id)}`)
      if (!alive) return
      jobs.value[id] = { id, job, lost: null }
      if (isJobFinished(job.status)) {
        opts.onFinish?.(job)
        return
      }
      failures = 0
    } catch (err) {
      if (!alive) return
      const status = err instanceof ApiError ? err.status : 0
      const current = jobs.value[id]
      if (status === 404) {
        // Jobs are kept on disk; a 404 means it was pruned or never existed.
        jobs.value[id] = { id, job: current?.job ?? null, lost: 'The agent no longer knows this job. Check the Activity page or the backups list.' }
        return
      }
      if (status === 401 || failures >= 5) {
        jobs.value[id] = { id, job: current?.job ?? null, lost: `Stopped checking progress: ${errorMessage(err)}` }
        return
      }
      failures++
    }
    timers.set(id, setTimeout(() => poll(id, failures), interval))
  }

  function watch(id: string) {
    jobs.value[id] = { id, job: null, lost: null }
    poll(id)
  }

  function dismiss(id: string) {
    const t = timers.get(id)
    if (t) clearTimeout(t)
    timers.delete(id)
    const next = { ...jobs.value }
    delete next[id]
    jobs.value = next
  }

  onBeforeUnmount(() => {
    alive = false
    for (const t of timers.values()) clearTimeout(t)
    timers.clear()
  })

  return { jobs, watch, dismiss }
}
