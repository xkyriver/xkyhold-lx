<script lang="ts">
  // A newer version is out (docs/UPDATES.md 4.3): Update, What's new, Not now.
  import { i18n, t } from '../lib/i18n.svelte'
  import { app, skipUpdate, startUpdate } from '../state/app.svelte'

  let { withNotNow = true }: { withNotNow?: boolean } = $props()
  let showNotes = $state(false)

  const update = $derived(app.update!)
  const notes = $derived(i18n.lang === 'pt-PT' && update.notes_pt ? update.notes_pt : update.notes_en)
</script>

<div class="banner info" role="status">
  <span>
    {t('XkyHold-lx {version} is available.', { version: update.version })}
    {#if app.job}<span class="small muted">{t('You can update when this finishes.')}</span>{/if}
  </span>
  <button class="btn primary" onclick={startUpdate} disabled={!!app.job}>{t('Update')}</button>
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

<style>
  .notes {
    white-space: pre-line;
    margin: 0;
  }
</style>
