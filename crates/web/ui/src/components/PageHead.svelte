<script lang="ts">
  import type { Snippet } from 'svelte';
  let { eyebrow, title, info, children }: { eyebrow: string; title: string; info?: Snippet; children?: Snippet } = $props();
</script>

<header>
  <p class="eyebrow">{eyebrow}</p>
  <div class="title-row">
    <h1>{title}</h1>
    {#if info}
      <span class="info">
        <button type="button" class="info-trigger" aria-label="More information" aria-describedby="page-title-info">i</button>
        <span class="tooltip" id="page-title-info" role="tooltip">{@render info()}</span>
      </span>
    {/if}
  </div>
  {#if children}<p class="sub">{@render children()}</p>{/if}
</header>

<style>
  header { display: grid; gap: 3px; padding-bottom: 10px; border-bottom: 1px solid var(--color-border); }
  .eyebrow { font-size: 0.7rem; font-weight: 500; letter-spacing: 0.07em; text-transform: uppercase; color: var(--text-dim); }
  .title-row { display: flex; align-items: center; gap: 8px; }
  .info { position: relative; display: inline-flex; }
  .info-trigger {
    width: 20px; height: 20px; min-height: 20px; padding: 0;
    border: 1px solid var(--text-dim); border-radius: 50%;
    color: var(--text-dim); background: transparent;
    font-family: var(--font); font-size: 0.7rem; font-weight: 700;
  }
  .info-trigger:hover:not(:disabled), .info-trigger:focus-visible { color: var(--text); background: var(--hover-surface); filter: none; }
  .tooltip {
    position: absolute; z-index: 20; top: calc(100% + 9px); left: 50%;
    width: max-content; max-width: min(430px, calc(100vw - 40px)); padding: 9px 11px;
    border: 1px solid var(--color-border); border-radius: 7px;
    color: var(--text); background: var(--popover); box-shadow: var(--panel-shadow);
    font-size: 0.78rem; font-weight: 400; line-height: 1.45;
    opacity: 0; visibility: hidden; pointer-events: none; transform: translate(-50%, -3px);
    transition: opacity 0.15s, transform 0.15s, visibility 0.15s;
  }
  .info:hover .tooltip, .info:focus-within .tooltip { opacity: 1; visibility: visible; transform: translate(-50%, 0); }
  .sub { color: var(--text-dim); max-width: 64ch; font-size: 0.8125rem; }
</style>
