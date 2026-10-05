<script lang="ts">
  import { notify } from '../lib/app.svelte';

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
  <button type="button" class="sm" onclick={copy}>{copied ? 'Copied' : 'Copy'}</button>
</div>

<style>
  .copy { display: flex; gap: 10px; align-items: flex-start; }
  pre { flex: 1; min-width: 0; user-select: all; }
  button { margin-top: 8px; }
</style>
