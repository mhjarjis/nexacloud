// Types for the NexaCloud agent API (docs/API.md, v0.2).
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

export type JobStatus = 'queued' | 'running' | 'ok' | 'failed' | 'cancelled' | 'interrupted'

export interface Job {
  id: string
  kind: string
  title: string
  status: JobStatus
  log: string
  created_at: string
  /** When it started running (null while queued). */
  started_at: string | null
  finished_at: string | null
  heavy: boolean
  locks: string[]
  timeout_secs: number | null
  /** Whether it can be cancelled while running. */
  cancellable: boolean
  error: string | null
  change_id: string | null
  actor_ip: string | null
}

export interface Me {
  ok: boolean
  sudo_until: string | null
}

export type ChangeStatus =
  | 'pending' | 'applying' | 'applied' | 'failed' | 'reverted'
  | 'revert_failed' | 'rolled_back' | 'interrupted' | 'cancelled'

export interface FilePreview {
  path: string
  action: 'create' | 'modify' | 'delete' | 'permissions'
  diff: string
  mode: string | null
  owner: string | null
  old_sha256: string | null
  new_sha256: string | null
}

export type VerifyStep =
  | { kind: 'command'; argv: string[]; timeout_secs: number }
  | { kind: 'http'; url: string; expect_status: number; timeout_secs: number }

export interface VerifyResult {
  step: string
  ok: boolean
  detail: string
}

/** Response of an advisor `.../preview` endpoint. */
export interface ChangePreviewData {
  kind: string
  title: string
  target: string
  nothing_to_do: boolean
  note: string | null
  plan_hash: string
  locks: string[]
  files: FilePreview[]
  commands: string[][]
  verify: VerifyStep[]
  on_revert: string[][]
}

export interface ChangeSummary {
  id: string
  kind: string
  title: string
  target: string
  status: ChangeStatus
  created_at: string
  finished_at: string | null
  error: string | null
  job_id: string | null
  actor_ip: string | null
  rollback_of: string | null
  rolled_back_by: string | null
  plan_hash: string
  files_count: number
  commands_count: number
  can_rollback: boolean
}

export interface ChangeDetail extends ChangeSummary {
  note: string | null
  files: FilePreview[]
  commands: string[][]
  verify: VerifyStep[]
  on_revert: string[][]
  verify_results: VerifyResult[]
}

/** Response of apply / rollback endpoints. */
export interface ChangeStarted {
  change_id: string
  job_id: string
}

export interface AuditEntry {
  id: number
  ts: string
  ip: string | null
  action: string
  target: string | null
  change_id: string | null
  job_id: string | null
  plan_hash: string | null
  result: string
}

export interface SecurityInfo {
  ufw: { installed: boolean; active: boolean; rules: string[] }
  fail2ban: { installed: boolean; active: boolean }
  unattended_upgrades: boolean | null
  ssh_password_auth: boolean | null
}

export type Tone = 'ok' | 'warn' | 'bad' | 'info' | 'off'

// ---------------------------------------------------------------- Phase 1

export interface SiteOptions {
  client_max_body_size: string
  security_headers: boolean
  hsts: boolean
  cache_static: boolean
  block_hidden: boolean
}

export interface SiteModel {
  id: string
  kind: 'laravel' | 'php' | 'static' | 'proxy' | 'redirect'
  domains: string[]
  project_dir: string | null
  root: string | null
  php_version: string | null
  runtime: 'fpm' | 'frankenphp'
  runtime_port: number | null
  upstream: string | null
  redirect_to: string | null
  redirect_code: number | null
  ssl: boolean
  options: SiteOptions
  fpm_socket?: string | null
  user?: string | null
}

export interface PhpConfig {
  version: string
  loaded_modules: string[]
  optional_extensions: string[]
  ini: Record<string, string>
  ini_keys: { key: string; type: string }[]
}

export interface CatalogItem {
  id: string
  label: string
  description: string
  packages: string[]
  unit: string | null
  probe: string
  installed: boolean
}

export interface Finding {
  id: string
  severity: AlertLevel
  category: string
  title: string
  detail: string
  current: string | null
  recommended: string | null
  tier: 'safe' | 'restart' | 'manual'
  fixable: boolean
  link: string | null
  dismissed_until: string | null
}

export interface AdvisorReport {
  score: number
  findings: Finding[]
  dismissed: Finding[]
  ram_bytes: number
}

export interface BackupDestination {
  id: string
  name: string
  kind: 'local' | 'sftp' | 's3'
  config: Record<string, unknown>
  created_at: string
}

export interface BackupPlan {
  id: string
  name: string
  destination_id: string | null
  databases: string[]
  paths: string[]
  schedule: string
  keep_daily: number
  keep_weekly: number
  keep_monthly: number
  enabled: boolean
  last_run_at: string | null
  created_at: string
}

export interface BackupRunItem { kind: 'database' | 'files'; source: string; file: string; size_bytes: number }

export interface BackupRun {
  id: string
  plan_id: string
  job_id: string | null
  status: 'running' | 'ok' | 'failed' | 'interrupted'
  stamp: string
  started_at: string
  finished_at: string | null
  size_bytes: number
  items: BackupRunItem[]
  local_path: string | null
  remote_path: string | null
  error: string | null
}

export interface DiskUsageItem { path: string; label: string; bytes: number }
