<script lang="ts">
  import { dismiss, toasts } from '../lib/app.svelte';
  import { fly } from 'svelte/transition';
</script>

<div class="toasts" role="status" aria-live="polite">
  {#each toasts as toast (toast.id)}
    <div class="toast" class:ok={toast.ok} role={toast.ok ? undefined : 'alert'} transition:fly={{ y: 12, duration: 180 }}>
      <span class="mark" aria-hidden="true">{toast.ok ? '✓' : '!'}</span>
      <p>{toast.message}</p>
      <button type="button" class="icon sm" aria-label="Dismiss" onclick={() => dismiss(toast.id)}>×</button>
    </div>
  {/each}
</div>

<style>
  .toasts { position: fixed; right: 20px; bottom: 20px; display: grid; gap: 12px; z-index: 10; width: min(420px, calc(100vw - 32px)); }
  .toast {
    display: flex; gap: 12px; align-items: center;
    padding: 12px 12px 12px 16px;
    background: var(--color-surface);
    border: 1px solid var(--color-border);
    border-radius: 10px;
    box-shadow: var(--panel-shadow);
  }
  p { flex: 1; font-size: 0.9rem; }
  .mark { width: 26px; height: 26px; border: 1px solid currentColor; border-radius: 50%; display: grid; place-items: center; font-weight: 700; font-size: 0.8rem; color: var(--danger); flex: none; }
  .ok .mark { color: var(--ok); }
  .icon.sm { width: 30px; height: 30px; padding: 0; }
</style>
