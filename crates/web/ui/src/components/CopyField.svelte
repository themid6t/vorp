<script lang="ts">
  import { notify } from '../lib/app.svelte';
  import Icon from './Icon.svelte';

  let { value, label }: { value: string; label: string } = $props();
  let copied = $state(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
      copied = true;
      setTimeout(() => (copied = false), 1500);
    } catch {
      notify('Copying failed. Select the text and copy it by hand.');
    }
  }
</script>

<div class="copy">
  <pre class="code" aria-label={label}>{value}</pre>
  <button
    type="button"
    class="copy-button"
    onclick={copy}
    aria-label={copied ? `${label} copied` : `Copy ${label}`}
    title={copied ? 'Copied' : 'Copy'}
  >
    <Icon name={copied ? 'check' : 'copy'} size={17} />
  </button>
</div>

<style>
  .copy {
    min-width: 0; max-width: 100%; display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: stretch;
    border: 1px solid var(--color-border); border-radius: var(--radius-sm); background: var(--color-input);
    overflow: hidden;
  }
  pre.code {
    min-width: 0; max-width: 100%; padding: 11px 13px;
    border: 0; border-radius: 0; background: transparent;
    font-family: var(--mono); white-space: pre; overflow-x: auto; overflow-y: hidden; overflow-wrap: normal;
    scrollbar-width: thin; user-select: all;
  }
  .copy-button {
    align-self: center; width: 32px; height: 32px; min-height: 32px; margin: 5px 6px; padding: 0;
    color: var(--text-dim); background: var(--color-input); border-color: var(--color-border);
  }
  .copy-button:hover:not(:disabled) { color: var(--text); background: var(--hover-surface); filter: none; }
</style>
