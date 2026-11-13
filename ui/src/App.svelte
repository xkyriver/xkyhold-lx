<script lang="ts">
  // Phase 0 prototype: shows that the window renders the XkyHold design
  // and can call into the Rust engine. Replaced by the real screens in Phase 4.
  import { invoke } from '@tauri-apps/api/core'

  interface Hello {
    version: string
    engine_formats: number[]
  }

  let hello = $state<Hello | null>(null)
  let error = $state('')
  let clicks = $state(0)

  invoke<Hello>('hello').then(
    (h) => (hello = h),
    (e) => (error = String(e)),
  )
</script>

<div class="shell">
  <header class="topbar">
    <span class="brand"><img src="/icon.svg" alt="" width="28" height="28" />XkyHold-lx</span>
  </header>
  <main class="page">
    <section class="card stack">
      <h1>Your home folder, backed up</h1>
      <p class="muted">Phase 0 prototype window.</p>
      {#if hello}
        <p class="banner ok"><span>Engine {hello.version} reads format {hello.engine_formats.join(', ')}.</span></p>
      {:else if error}
        <p class="banner warn"><span>{error}</span></p>
      {:else}
        <p class="muted">Calling the engine…</p>
      {/if}
      <div class="row">
        <button class="btn primary" onclick={() => clicks++}>Click me</button>
        <span class="muted" data-testid="clicks">Clicked {clicks} times</span>
      </div>
    </section>
  </main>
</div>
