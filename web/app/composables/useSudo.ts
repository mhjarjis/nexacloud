// Sudo mode: dangerous actions (apply/rollback a change, restart a service,
// cancel a job) answer `403 {"error":"sudo_required"}` until the admin
// re-enters their password. `useApi()` calls `requestSudo()` on that error,
// the layout's <SudoPrompt> asks for the password inline, and the original
// request is retried automatically once it is accepted.

/** Thrown to the caller when the person dismisses the password prompt. */
export class SudoCancelled extends Error {
  constructor() {
    super('Cancelled: password not confirmed')
    this.name = 'SudoCancelled'
  }
}

interface SudoState {
  open: boolean
  busy: boolean
  error: string | null
  until: string | null
}

type Waiter = { resolve: () => void; reject: (e: unknown) => void }
let waiters: Waiter[] = []

export const useSudoState = () =>
  useState<SudoState>('nc-sudo', () => ({ open: false, busy: false, error: null, until: null }))

export function useSudo() {
  const state = useSudoState()

  /** Show the prompt; resolves once the password was accepted. */
  function requestSudo(): Promise<void> {
    state.value = { ...state.value, open: true, error: null }
    return new Promise<void>((resolve, reject) => waiters.push({ resolve, reject }))
  }

  async function submit(password: string): Promise<boolean> {
    if (!password || state.value.busy) return false
    state.value = { ...state.value, busy: true, error: null }
    try {
      const res = await $fetch<{ ok: boolean; sudo_until: string }>('/api/auth/sudo', {
        method: 'POST',
        body: { password },
        credentials: 'include',
        headers: { Accept: 'application/json' },
      })
      state.value = { open: false, busy: false, error: null, until: res.sudo_until }
      const w = waiters
      waiters = []
      w.forEach((x) => x.resolve())
      return true
    } catch (err: unknown) {
      const e = err as { response?: { status?: number }; status?: number; data?: { error?: string } }
      const status = e?.response?.status ?? e?.status ?? 0
      const msg = status === 401
        ? 'Wrong password. Try again.'
        : status === 429
          ? 'Too many attempts from this address. Wait 15 minutes, then try again.'
          : (typeof e?.data?.error === 'string' ? e.data.error : 'Could not reach the NexaCloud agent')
      state.value = { ...state.value, busy: false, error: msg }
      return false
    }
  }

  function cancel() {
    state.value = { ...state.value, open: false, busy: false, error: null }
    const w = waiters
    waiters = []
    w.forEach((x) => x.reject(new SudoCancelled()))
  }

  return { state, requestSudo, submit, cancel }
}
