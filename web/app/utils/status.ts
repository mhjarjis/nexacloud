// Map raw API fields to labels and pill tones. Auto-imported from app/utils.
import type { AlertLevel, ChangeStatus, Job, PhpVersion, Service, Site, SiteKind, Tone } from '~/types/api'

export interface Status {
  tone: Tone
  label: string
}

export function alertTone(level: AlertLevel): Status {
  if (level === 'critical') return { tone: 'bad', label: 'Critical' }
  if (level === 'warning') return { tone: 'warn', label: 'Warning' }
  return { tone: 'info', label: 'Info' }
}

const KIND_LABELS: Record<SiteKind, string> = {
  laravel: 'Laravel',
  php: 'PHP',
  static: 'Static',
  proxy: 'Proxy',
  unknown: 'Unknown',
}

export function siteKindLabel(kind: SiteKind | string): string {
  return KIND_LABELS[kind as SiteKind] ?? 'Unknown'
}

/** SSL state of a site; warns under 14 days like the agent's alert rule. */
export function sslStatus(site: Pick<Site, 'ssl' | 'ssl_expires'>, now = Date.now()): Status {
  if (!site.ssl) return { tone: 'off', label: 'No SSL' }
  const days = daysUntil(site.ssl_expires, now)
  if (days == null) return { tone: 'warn', label: 'Expiry unknown' }
  if (days < 0) return { tone: 'bad', label: 'Expired' }
  if (days === 0) return { tone: 'bad', label: 'Expires today' }
  if (days < 14) return { tone: 'warn', label: `${days} day${days === 1 ? '' : 's'} left` }
  return { tone: 'ok', label: `${days} days left` }
}

export function serviceStatus(svc: Pick<Service, 'installed' | 'active' | 'state'>): Status {
  if (!svc.installed) return { tone: 'off', label: 'Not installed' }
  if (svc.active) return { tone: 'ok', label: 'Running' }
  const state = svc.state && svc.state !== 'active' ? svc.state : 'stopped'
  return { tone: 'bad', label: state.charAt(0).toUpperCase() + state.slice(1) }
}

export function jobStatus(status: Job['status']): Status {
  switch (status) {
    case 'queued': return { tone: 'off', label: 'Queued' }
    case 'running': return { tone: 'info', label: 'Running' }
    case 'ok': return { tone: 'ok', label: 'Done' }
    case 'failed': return { tone: 'bad', label: 'Failed' }
    case 'cancelled': return { tone: 'off', label: 'Cancelled' }
    case 'interrupted': return { tone: 'warn', label: 'Interrupted' }
    default: return { tone: 'off', label: String(status) }
  }
}

export function isJobFinished(status: Job['status']): boolean {
  return status !== 'queued' && status !== 'running'
}

export function changeStatus(status: ChangeStatus): Status {
  switch (status) {
    case 'pending': return { tone: 'off', label: 'Queued' }
    case 'applying': return { tone: 'info', label: 'Applying' }
    case 'applied': return { tone: 'ok', label: 'Applied' }
    case 'failed': return { tone: 'bad', label: 'Failed' }
    case 'reverted': return { tone: 'warn', label: 'Reverted' }
    case 'revert_failed': return { tone: 'bad', label: 'Revert failed' }
    case 'rolled_back': return { tone: 'off', label: 'Rolled back' }
    case 'interrupted': return { tone: 'warn', label: 'Interrupted' }
    case 'cancelled': return { tone: 'off', label: 'Cancelled' }
    default: return { tone: 'off', label: String(status) }
  }
}

export function isChangeFinished(status: ChangeStatus): boolean {
  return status !== 'pending' && status !== 'applying'
}

/** `["nginx", "-t"]` → `nginx -t` (args with spaces quoted). */
export function displayArgv(argv: string[]): string {
  return argv.map((a) => (a === '' || /[\s']/.test(a) ? `'${a.replace(/'/g, `'\\''`)}'` : a)).join(' ')
}

export interface PhpSupport extends Status {
  /** One plain-English line about the support window. */
  detail: string
}

/**
 * PHP support phase. The contract gives `eol` and `security_until` only; PHP's
 * policy is two years of active support followed by two years of security
 * fixes, so active support is taken to end two years before `security_until`.
 */
export function phpSupport(v: Pick<PhpVersion, 'eol' | 'security_until'>, now = Date.now()): PhpSupport {
  const until = v.security_until
  // The agent's `eol` flag is authoritative for end of life.
  if (v.eol) {
    return {
      tone: 'bad',
      label: 'End of life',
      detail: until ? `No security fixes since ${formatDate(until)}` : 'No longer receives security fixes',
    }
  }
  // `security_until` is a calendar date: support lasts through the whole day.
  const securityEnd = endOfUtcDay(until)
  if (!until || !securityEnd) {
    return { tone: 'off', label: 'Unknown', detail: 'Support window unknown' }
  }
  const activeEnd = new Date(securityEnd)
  activeEnd.setUTCFullYear(activeEnd.getUTCFullYear() - 2)
  if (now <= activeEnd.getTime()) {
    const activeDate = activeEnd.toISOString().slice(0, 10)
    return { tone: 'ok', label: 'Active', detail: `Bug and security fixes until ${formatDate(activeDate)}` }
  }
  const days = Math.max(0, Math.floor((securityEnd.getTime() - now) / 86_400_000))
  if (days < 180) {
    return {
      tone: 'warn',
      label: 'Security only',
      detail: `Security fixes end ${formatDate(until)} (${days === 0 ? 'today' : `in ${days} day${days === 1 ? '' : 's'}`})`,
    }
  }
  return { tone: 'info', label: 'Security only', detail: `Security fixes until ${formatDate(until)}` }
}

export function fpmStatus(v: Pick<PhpVersion, 'fpm_installed' | 'fpm_active'>): Status {
  if (!v.fpm_installed) return { tone: 'off', label: 'No FPM' }
  if (v.fpm_active) return { tone: 'ok', label: 'FPM running' }
  return { tone: 'bad', label: 'FPM stopped' }
}
