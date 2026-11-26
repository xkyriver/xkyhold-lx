// The engine, as the window sees it: Tauri commands and events. Errors
// arrive as { kind, message }; `explain` in the app state turns the kind
// into a sentence.

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type { UnlistenFn } from '@tauri-apps/api/event'

export type Target = { kind: 'drive'; name: string } | { kind: 'local'; path: string }

export interface What {
  only: string[]
  leave_out: string[]
  presets: string[]
  patterns: string[]
  cross_mounts: string[]
}

export interface LastBackup {
  time: string
  files: number
  bytes: number
  skipped: number
}

export interface Settings {
  account: string | null
  backup: Target | null
  repo_id: string | null
  what: What
  remind_days: number
  keep_months: number
  language: string | null
  theme: string
  last_backup: LastBackup | null
  automatic: 'off' | 'hourly' | 'daily' | 'weekly'
  notify_success: boolean
  check_updates: boolean
  update_checked: string | null
  update_latest: Available | null
  update_skipped: string | null
}

/** A newer version of XkyHold-lx (docs/UPDATES.md 4.3). */
export interface Available {
  version: string
  page: string
  notes_en: string
  notes_pt: string
}

export interface Status {
  version: string
  home: string
  settings: Settings
  configured: boolean
  open: boolean
  writable: boolean
  remembered: boolean
  job: string | null
  running_here: boolean
  timer: boolean
  google_client: boolean
  keyring: boolean
  /** How "Update" works here (docs/UPDATES.md 4.4). */
  install: 'appimage' | 'appman' | 'deb' | 'rpm' | 'other'
  /** This session may turn the computer off. */
  can_power_off: boolean
}

export interface SnapInfo {
  id: string
  time: string
  device: string
  files: number
  bytes: number
  skipped: number
  home: boolean
}

export interface EntryInfo {
  id: string
  name: string
  kind: 'file' | 'dir' | 'mount' | 'symlink' | 'unknown'
  size: number
  mtime: number | null
}

export interface FolderInfo {
  name: string
  text_name: boolean
  mount: boolean
  cloud: boolean
  always_out: boolean
}

export interface Secret {
  password?: string
  recovery_key?: string
}

export interface UiError {
  kind: string
  message: string
}

export interface ProgressEvent {
  kind: 'backup' | 'cleanup' | 'restore' | 'check'
  files: number
  bytes: number
  current: string
}

export interface JobDone {
  kind: 'backup' | 'restore' | 'check'
  ok: boolean
  error?: UiError
  summary?: Record<string, number | string | boolean | null>
  /** A backup that went well, after which the person asked to turn the computer off. */
  shutdown?: boolean
}

export type FolderState = 'missing' | 'empty' | 'repository' | 'other' | 'contains-home'

export const api = {
  status: () => invoke<Status>('status'),
  openSaved: () => invoke<boolean>('open_saved'),
  signIn: () => invoke<string>('sign_in'),
  signOut: () => invoke<void>('sign_out'),
  cancelSignIn: () => invoke<void>('cancel_sign_in'),
  driveBackups: () => invoke<string[]>('drive_backups'),
  inspectFolder: (path: string) => invoke<FolderState>('inspect_folder', { path }),
  pickFolder: () => invoke<string | null>('pick_folder'),
  createBackup: (target: Target, password: string, remember: boolean) =>
    invoke<string>('create_backup', { target, password, remember }),
  connectBackup: (target: Target, secret: Secret, remember: boolean) =>
    invoke<boolean>('connect_backup', { target, secret, remember }),
  unlock: (secret: Secret, remember: boolean) => invoke<void>('unlock', { secret, remember }),
  backupNow: () => invoke<void>('backup_now'),
  stopJob: () => invoke<void>('stop_job'),
  snapshots: () => invoke<SnapInfo[]>('snapshots'),
  entries: (snapshot: string, path: string[]) => invoke<EntryInfo[]>('entries', { snapshot, path }),
  restoreExisting: (snapshot: string, path: string[], ids: string[]) =>
    invoke<string[]>('restore_existing', { snapshot, path, ids }),
  /** `to`: a folder the user chose, instead of ~/XkyHold-restore; null for the default. */
  restoreStart: (
    snapshot: string,
    path: string[],
    ids: string[],
    original: boolean,
    folder: string,
    to: string | null,
    conflict: string,
  ) => invoke<void>('restore_start', { snapshot, path, ids, original, folder, to, conflict }),
  checkStart: (full: boolean) => invoke<void>('check_start', { full }),
  saveSettings: (
    patch: Partial<
      Pick<Settings, 'what' | 'remind_days' | 'keep_months' | 'language' | 'theme' | 'automatic' | 'notify_success' | 'check_updates'>
    >,
  ) =>
    invoke<Settings>('save_settings', { patch }),
  changePassword: (secret: Secret, newPassword: string) => invoke<void>('change_password', { secret, newPassword }),
  setRemember: (remember: boolean) => invoke<void>('set_remember', { remember }),
  forgetComputer: () => invoke<void>('forget_computer'),
  folders: (path: string) => invoke<FolderInfo[]>('folders', { path }),
  mounts: () => invoke<string[]>('mounts'),
  countSizes: () => invoke<void>('count_sizes'),
  openFolder: (path: string) => invoke<void>('open_folder', { path }),
  closeNow: () => invoke<void>('close_now'),
  updateCheck: (now: boolean) => invoke<Available | null>('update_check', { now }),
  updateSkip: (version: string) => invoke<void>('update_skip', { version }),
  openRelease: (url: string) => invoke<void>('open_release', { url }),
  updateInstall: () => invoke<boolean>('update_install'),
  updateRestart: () => invoke<void>('update_restart'),
  setShutdownAfter: (on: boolean) => invoke<void>('set_shutdown_after', { on }),
  powerOff: () => invoke<void>('power_off'),
}

export const on = {
  progress: (f: (p: ProgressEvent) => void): Promise<UnlistenFn> => listen<ProgressEvent>('progress', (e) => f(e.payload)),
  jobDone: (f: (d: JobDone) => void): Promise<UnlistenFn> => listen<JobDone>('job-done', (e) => f(e.payload)),
  sizes: (f: (s: Record<string, number>) => void): Promise<UnlistenFn> =>
    listen<Record<string, number>>('sizes', (e) => f(e.payload)),
  signinUrl: (f: (url: string) => void): Promise<UnlistenFn> => listen<string>('signin-url', (e) => f(e.payload)),
  closeRequested: (f: () => void): Promise<UnlistenFn> => listen('close-requested', () => f()),
  updateProgress: (f: (p: { done: number; total: number }) => void): Promise<UnlistenFn> =>
    listen<{ done: number; total: number }>('update-progress', (e) => f(e.payload)),
}
