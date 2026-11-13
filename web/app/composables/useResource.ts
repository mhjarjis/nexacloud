// Fetch one API resource with loading/error state and optional polling.
// `loading` is only true until the first response, so periodic refreshes
// don't flash skeletons.

interface ResourceOptions {
  /** Refresh interval in ms (paused while the tab is hidden). */
  interval?: number
}

export function useResource<T>(path: string, opts: ResourceOptions = {}) {
  const api = useApi()
  const data = shallowRef<T | null>(null)
  const error = ref<string | null>(null)
  const loading = ref(true)
  const refreshing = ref(false)
  let timer: ReturnType<typeof setInterval> | undefined
  let inflight: Promise<void> | null = null
  let alive = true

  function refresh(): Promise<void> {
    if (inflight) return inflight
    refreshing.value = true
    inflight = (async () => {
      try {
        const res = await api.get<T>(path)
        if (!alive) return
        data.value = res
        error.value = null
      } catch (err) {
        if (!alive) return
        error.value = errorMessage(err)
      } finally {
        loading.value = false
        refreshing.value = false
        inflight = null
      }
    })()
    return inflight
  }

  onMounted(() => {
    refresh()
    if (opts.interval) {
      timer = setInterval(() => {
        if (typeof document !== 'undefined' && document.hidden) return
        refresh()
      }, opts.interval)
    }
  })

  onBeforeUnmount(() => {
    alive = false
    if (timer) clearInterval(timer)
  })

  return { data, error, loading, refreshing, refresh }
}
