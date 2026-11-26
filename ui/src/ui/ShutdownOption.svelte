<script lang="ts">
  // "Turn off the computer when the backup finishes", for this backup only.
  import { t } from '../lib/i18n.svelte'
  import { app, setShutdownAfter } from '../state/app.svelte'
</script>

{#if app.status?.can_power_off}
  <label class="check small">
    <input type="checkbox" checked={app.shutdownAfter} onchange={(e) => setShutdownAfter(e.currentTarget.checked)} />
    {t('Turn off the computer when the backup finishes')}
  </label>
  {#if app.shutdownAfter}
    <p class="small muted">
      {t('Only if the backup goes well, after a minute to cancel. Do not close the lid of a laptop: closing it still puts the computer to sleep.')}
    </p>
  {/if}
{/if}

<style>
  .check {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    justify-content: center;
  }
</style>
