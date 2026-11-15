<script lang="ts">
  // A newer version is out (docs/UPDATES.md 4.3): Update, What's new, Not now;
  // then the download, and Restart now.
  import { formatBytes } from '../lib/format'
  import { i18n, t } from '../lib/i18n.svelte'
  import { app, restartAfterUpdate, skipUpdate, startUpdate, updatesInPlace } from '../state/app.svelte'

  let { withNotNow = true }: { withNotNow?: boolean } = $props()
  let showNotes = $state(false)

  const update = $derived(app.update!)
  const notes = $derived(i18n.lang === 'pt-PT' && update.notes_pt ? update.notes_pt : update.notes_en)
  const inPlace = $derived(updatesInPlace())
</script>

{#if app.updating?.ready}
  <div class="banner ok" role="status">
    <span>{t('XkyHold-lx {version} is installed. Restart to use it.', { version: update.version })}</span>
    <button class="btn primary" onclick={restartAfterUpdate} disabled={!!app.job}>{t('Restart now')}</button>
  </div>
{:else if app.updating}
  <div class="banner info" role="status">
    <span>
      {#if app.updating.total && app.updating.done >= app.updating.total}
        {t('Checking the signature…')}
      {:else if app.updating.total}
        {t('Downloading XkyHold-lx {version}: {done} of {total}', {
          version: update.version,
          done: formatBytes(app.updating.done),
          total: formatBytes(app.updating.total),
        })}
      {:else}
        {t('Getting ready…')}
      {/if}
    </span>
  </div>
{:else}
  <div class="banner info" role="status">
    <span>
      {t('XkyHold-lx {version} is available.', { version: update.version })}
      {#if app.job}<span class="small muted">{t('You can update when this finishes.')}</span>{/if}
      {#if !inPlace}<span class="small muted">{t('Download it from the release page and install it as you did before.')}</span>{/if}
    </span>
    <button class="btn primary" onclick={startUpdate} disabled={!!app.job}>{inPlace ? t('Update') : t('Open the release page')}</button>
    {#if notes}
      <button class="link" onclick={() => (showNotes = !showNotes)}>{t('What’s new')}</button>
    {/if}
    {#if withNotNow}
      <button class="btn" onclick={skipUpdate}>{t('Not now')}</button>
    {/if}
  </div>
  {#if showNotes && notes}
    <p class="notes small">{notes}</p>
  {/if}
{/if}

<style>
  .notes {
    white-space: pre-line;
    margin: 0;
  }
</style>
