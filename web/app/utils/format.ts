// Formatting helpers (auto-imported by Nuxt from app/utils).

const DAY_MS = 86_400_000

export function formatBytes(bytes: number | null | undefined, digits = 1): string {
  if (bytes == null || !Number.isFinite(bytes)) return '—'
  if (bytes < 1024) return `${bytes} B`
  const units = ['KB', 'MB', 'GB', 'TB', 'PB']
  let value = bytes / 1024
  let i = 0
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024
    i++
  }
  const d = value >= 100 ? 0 : digits
  return `${value.toFixed(d)} ${units[i]}`
}

/** "3 d 4 h", "5 h 12 m", "42 s" — for uptimes and job durations. */
export function formatDuration(totalSecs: number | null | undefined): string {
  if (totalSecs == null || !Number.isFinite(totalSecs) || totalSecs < 0) return '—'
  const s = Math.floor(totalSecs)
  const d = Math.floor(s / 86_400)
  const h = Math.floor((s % 86_400) / 3600)
  const m = Math.floor((s % 3600) / 60)
  const sec = s % 60
  if (d > 0) return h > 0 ? `${d} d ${h} h` : `${d} d`
  if (h > 0) return m > 0 ? `${h} h ${m} m` : `${h} h`
  if (m > 0) return sec > 0 && m < 10 ? `${m} m ${sec} s` : `${m} m`
  return `${sec} s`
}

const DATE_ONLY = /^\d{4}-\d{2}-\d{2}$/

/** True for calendar dates without a time ("2026-12-31"). */
export function isDateOnly(value: string | null | undefined): boolean {
  return !!value && DATE_ONLY.test(value)
}

function toDate(iso: string | null | undefined): Date | null {
  if (!iso) return null
  const d = new Date(iso)
  return Number.isNaN(d.getTime()) ? null : d
}

/** End (23:59:59.999 UTC) of a calendar date such as "2026-12-31"; null if unparsable. */
export function endOfUtcDay(date: string | null | undefined): Date | null {
  if (!date) return null
  const d = toDate(isDateOnly(date) ? `${date}T23:59:59.999Z` : date)
  return d
}

/** "just now", "5 min ago", "in 3 days" */
export function formatRelative(iso: string | null | undefined, now: number = Date.now()): string {
  const d = toDate(iso)
  if (!d) return '—'
  const diff = d.getTime() - now
  const abs = Math.abs(diff)
  const future = diff > 0
  const wrap = (n: number, unit: string) => {
    const label = `${n} ${unit}${n === 1 ? '' : 's'}`
    return future ? `in ${label}` : `${label} ago`
  }
  if (abs < 45_000) return future ? 'in a moment' : 'just now'
  if (abs < 3_600_000) return wrap(Math.round(abs / 60_000), 'min')
  if (abs < DAY_MS) return wrap(Math.round(abs / 3_600_000), 'hour')
  if (abs < 30 * DAY_MS) return wrap(Math.round(abs / DAY_MS), 'day')
  if (abs < 365 * DAY_MS) return wrap(Math.round(abs / (30 * DAY_MS)), 'month')
  return wrap(Math.round(abs / (365 * DAY_MS)), 'year')
}

/** "13 Nov 2026" */
export function formatDate(iso: string | null | undefined): string {
  const d = toDate(iso)
  if (!d) return '—'
  // Calendar dates (no time) are UTC midnight; show them as-is in every timezone.
  return d.toLocaleDateString(undefined, {
    day: 'numeric', month: 'short', year: 'numeric', ...(isDateOnly(iso) ? { timeZone: 'UTC' } : {}),
  })
}

/** "13 Nov 2026, 03:00" */
export function formatDateTime(iso: string | null | undefined): string {
  const d = toDate(iso)
  if (!d) return '—'
  return d.toLocaleString(undefined, {
    day: 'numeric', month: 'short', year: 'numeric', hour: '2-digit', minute: '2-digit',
  })
}

/** Whole days from now until the given date (negative when in the past). */
export function daysUntil(iso: string | null | undefined, now: number = Date.now()): number | null {
  const d = toDate(iso)
  if (!d) return null
  return Math.floor((d.getTime() - now) / DAY_MS)
}

export function percent(used: number, total: number): number {
  if (!total) return 0
  return Math.min(100, Math.max(0, (used / total) * 100))
}

export function secondsBetween(start: string | null | undefined, end: string | null | undefined): number | null {
  const a = toDate(start)
  if (!a) return null
  const b = toDate(end) ?? new Date()
  return Math.max(0, (b.getTime() - a.getTime()) / 1000)
}
