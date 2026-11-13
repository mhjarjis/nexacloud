// Single entry point for talking to the NexaCloud agent.
// Every request sends the session cookie; a 401 sends the user to /login.

export class ApiError extends Error {
  status: number
  constructor(message: string, status: number) {
    super(message)
    this.name = 'ApiError'
    this.status = status
  }
}

interface RequestOptions {
  method?: 'GET' | 'POST'
  body?: Record<string, unknown>
  /** Don't redirect to /login on 401 (used by the login form itself). */
  allowUnauthorized?: boolean
}

/** Whether the current browser session is signed in (null = not checked yet). */
export const useAuthState = () => useState<boolean | null>('nc-authed', () => null)

let redirecting = false

export function errorMessage(err: unknown): string {
  if (err instanceof ApiError) return err.message
  if (err instanceof Error) return err.message
  return 'Something went wrong'
}

export function useApi() {
  const authed = useAuthState()
  const router = useRouter()

  async function request<T>(path: string, opts: RequestOptions = {}): Promise<T> {
    try {
      return await $fetch<T>(path, {
        method: opts.method ?? 'GET',
        body: opts.body,
        credentials: 'include',
        headers: { Accept: 'application/json' },
      })
    } catch (err: unknown) {
      const e = err as { status?: number; statusCode?: number; response?: { status?: number }; data?: unknown }
      const status = e?.response?.status ?? e?.status ?? e?.statusCode ?? 0
      const data = e?.data as { error?: unknown } | undefined
      const serverMsg = data && typeof data.error === 'string' ? data.error : null
      const message = serverMsg
        ?? (status ? `Request failed (HTTP ${status})` : 'Could not reach the NexaCloud agent')

      if (status === 401 && !opts.allowUnauthorized) {
        authed.value = false
        if (!redirecting && router.currentRoute.value.path !== '/login') {
          redirecting = true
          try {
            await navigateTo('/login')
          } finally {
            redirecting = false
          }
        }
      }
      throw new ApiError(message, status)
    }
  }

  return {
    request,
    get: <T>(path: string) => request<T>(path),
    post: <T>(path: string, body?: Record<string, unknown>) => request<T>(path, { method: 'POST', body }),
  }
}
