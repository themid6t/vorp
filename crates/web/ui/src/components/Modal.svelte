<script lang="ts">
  import type { Snippet } from 'svelte';

  // A native modal dialog: focus trapping, Escape and the backdrop come from the browser.
  // `dismissible = false` ignores Escape, for dialogs that must be acknowledged.
  let { open = $bindable(false), title, eyebrow = '', dismissible = true, children }: {
    open?: boolean; title: string; eyebrow?: string; dismissible?: boolean; children: Snippet;
  } = $props();
  let dialog: HTMLDialogElement;

  $effect(() => {
    if (open && !dialog.open) dialog.showModal();
    else if (!open && dialog.open) dialog.close();
  });
</script>

<dialog bind:this={dialog} onclose={() => (open = false)}
  oncancel={(e) => { if (!dismissible) e.preventDefault(); }} aria-labelledby="modal-title">
  {#if open}
    <div class="body">
      <header>
        {#if eyebrow}<p class="eyebrow">{eyebrow}</p>{/if}
        <h2 id="modal-title">{title}</h2>
      </header>
      {@render children()}
    </div>
  {/if}
</dialog>

<style>
  dialog {
    border: 0;
    padding: 0;
    width: min(520px, calc(100vw - 32px));
    max-height: calc(100vh - 48px);
    background: var(--bg);
    color: var(--text);
    border-radius: 24px;
    box-shadow: 14px 14px 36px rgba(0, 0, 0, 0.28), -10px -10px 30px var(--hi);
    outline: 1px solid var(--outline);
  }
  dialog::backdrop { background: rgba(10, 14, 20, 0.45); backdrop-filter: blur(3px); }
  dialog[open] { animation: rise 0.18s ease-out; }
  .body { padding: 28px; display: grid; gap: 18px; }
  header { display: grid; gap: 4px; }
  h2 { font-size: 1.3rem; }
  .eyebrow { font-size: 0.75rem; font-weight: 600; letter-spacing: 0.08em; text-transform: uppercase; color: var(--accent-ink); }
  @keyframes rise { from { opacity: 0; transform: translateY(8px) scale(0.98); } }
</style>
