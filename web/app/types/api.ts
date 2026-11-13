// Types for the NexaCloud agent API (docs/API.md, v0.1).
// Fields the agent may report as unknown are typed nullable on purpose.

export interface Health {
  ok: boolean
  version: string
}

export type AlertLevel = 'critical' | 'warning' | 'info'

export interface Alert {
  level: AlertLevel
  title: string
  detail?: string | null
  link?: string | null
}

export interface Overview {
  hostname: string
  alerts: Alert[]
  counts: { sites: number; databases: number; services_down: number }
}

export interface UsedTotal {
  total: number
  used: number
}

export interface Disk extends UsedTotal {
  mount: string
}

export interface SystemInfo {
  hostname: string
  os: string
  kernel: string
  uptime_secs: number
  cpu_count: number
  cpu_percent: number
  load: number[]
  memory: UsedTotal
  swap: UsedTotal
  disks: Disk[]
}

export interface Service {
  unit: string
  label: string
  installed: boolean
  active: boolean
  state: string
  version: string | null
}

export interface PhpVersion {
  version: string
  fpm_installed: boolean
  fpm_active: boolean
  eol: boolean
  security_until: string | null
  sites: string[]
}

export interface PhpInfo {
  cli_default: string | null
  versions: PhpVersion[]
}

export type SiteKind = 'laravel' | 'php' | 'static' | 'proxy' | 'unknown'

export interface GitInfo {
  branch: string | null
  commit: string | null
  message: string | null
  remote: string | null
}

export interface Site {
  id: string
  server_names: string[]
  root: string | null
  kind: SiteKind
  php_version: string | null
  ssl: boolean
  ssl_expires: string | null
  proxy_pass: string | null
  config_path: string
  git: GitInfo | null
}

export interface DatabaseEntry {
  name: string
  size_bytes: number
  tables: number
}

export interface EngineInfo {
  available: boolean
  version: string | null
  databases: DatabaseEntry[]
  /** Why an installed engine can't be reached; null when it's simply not installed. */
  error?: string | null
}

export type Engine = 'mysql' | 'postgres'

export interface DatabasesInfo {
  mysql: EngineInfo
  postgres: EngineInfo
}

export interface Backup {
  engine: Engine | string
  database: string
  file: string
  size_bytes: number
  created_at: string
}

export type JobStatus = 'queued' | 'running' | 'ok' | 'failed'

export interface Job {
  id: string
  kind: string
  title: string
  status: JobStatus
  log: string
  started_at: string | null
  finished_at: string | null
}

export interface SecurityInfo {
  ufw: { installed: boolean; active: boolean; rules: string[] }
  fail2ban: { installed: boolean; active: boolean }
  unattended_upgrades: boolean | null
  ssh_password_auth: boolean | null
}

export type Tone = 'ok' | 'warn' | 'bad' | 'info' | 'off'
