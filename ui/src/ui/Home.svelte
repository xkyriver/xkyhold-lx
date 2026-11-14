<script lang="ts">
  import { formatBytes } from '../lib/format'
  import { fileCount, formatWhen, t } from '../lib/i18n.svelte'
  import { reminder } from '../lib/reminder'
  import { app, backupNow, openBackup, stopJob } from '../state/app.svelte'
  import Unlock from './Unlock.svelte'

  const status = $derived(app.status!)
  const last = $derived(status.settings.last_backup)
  const working = $derived(app.job?.kind === 'backup')
  const overdue = $derived(!!last && reminder(last.time, status.settings.remind_days).due)
  const where = $derived(
    status.settings.backup?.kind === 'drive'
      ? t('Google Drive · {name}', { name: status.settings.backup.name })
      : status.settings.backup?.path ?? '',
  )
</script>

{#if app.needsPassword && !status.open}
  <Unlock />
{:else}
  <section class="card center stack">
    <div class="mark" class:ok={!!last && !working && !overdue} class:due={overdue && !working} class:busy={working} aria-hidden="true">
      {#if working}
        <span class="spinner"></span>
      {:else if overdue}
        !
      {:else if last}
        ✓
      {:else}
        ↑
      {/if}
    </div>

    {#if working && app.job?.cleaning}
      <h1>{t('Removing old backups…')}</h1>
      <p class="small muted">{t('Your new backup is already safe.')}</p>
    {:else if working && app.job}
      <h1>{t('Backing up…')}</h1>
      {#if app.job.files === 0 && !app.job.current}
        <p class="muted">{t('Getting ready…')}</p>
      {:else}
        <p class="muted">{fileCount(app.job.files)} · {formatBytes(app.job.bytes)}</p>
      {/if}
      <p class="small muted current">{app.job.current || ' '}</p>
      <div><button class="btn" onclick={stopJob}>{t('Stop')}</button></div>
      <p class="small muted">{t('Keep this window open until the backup finishes. If it stops, the next backup continues where it left off.')}</p>
    {:else}
      {#if app.opening}
        <h1>{t('Opening your backup…')}</h1>
      {:else if last && overdue}
        <h1>{t('Time for a backup')}</h1>
      {:else if last}
        <h1>{t('Your files are safe')}</h1>
      {:else}
        <h1>{t('No backup yet')}</h1>
        <p class="muted">{t('Make your first backup now.')}</p>
      {/if}
      {#if last}
        <p class="muted">
          {t('Last backup: {when}', { when: formatWhen(last.time) })}<br />
          {fileCount(last.files)} · {formatBytes(last.bytes)}
        </p>
      {/if}
      {#if status.open && !status.writable}
        <p class="small muted">
          {t('This backup was made by XkyHold in a browser. You can restore from it, but new backups need a backup made by XkyHold-lx.')}
        </p>
      {:else if status.open}
        <div>
          <button class="btn primary big" onclick={backupNow} disabled={!!app.job || app.opening}>{t('Back up now')}</button>
        </div>
      {:else if !app.opening}
        <div><button class="btn primary" onclick={openBackup}>{t('Try again')}</button></div>
      {/if}
      <p class="small muted">{where}</p>
    {/if}
  </section>

  {#if last?.skipped && !working}
    <p class="small muted">
      {last.skipped === 1
        ? t('1 file could not be read in the last backup.')
        : t('{n} files could not be read in the last backup.', { n: last.skipped })}
    </p>
  {/if}
{/if}

<style>
  .mark {
    width: 64px;
    height: 64px;
    margin: 4px auto 0;
    display: grid;
    place-items: center;
    font-size: 1.75rem;
    font-weight: 700;
    color: var(--muted);
    background: var(--surface-2);
    border-radius: 50%;
  }

  .mark.ok {
    color: var(--ok);
    background: var(--ok-soft);
  }

  .mark.due {
    color: var(--warn);
    background: var(--warn-soft);
  }

  .mark.busy {
    background: var(--accent-soft);
  }

  .spinner {
    width: 28px;
    height: 28px;
    border: 3px solid var(--accent);
    border-right-color: transparent;
    border-radius: 50%;
    animation: spin 0.9s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .spinner {
      animation-duration: 4s;
    }
  }

  .current {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
