<script lang="ts">
  import { confirmation } from '../lib/app.svelte';
  import Modal from './Modal.svelte';

  function finish(ok: boolean) {
    confirmation.current?.resolve(ok);
    confirmation.current = null;
  }
</script>

<!-- Closing with Escape counts as "no". -->
<Modal bind:open={() => confirmation.current !== null, (v) => { if (!v && confirmation.current) finish(false); }} title={confirmation.current?.title ?? ''} eyebrow="confirm">
  <p class="dim">{confirmation.current?.message}</p>
  <div class="row end">
    <button type="button" onclick={() => finish(false)}>Cancel</button>
    <button type="button" class={confirmation.current?.danger ? 'danger' : 'primary'} onclick={() => finish(true)}>
      {confirmation.current?.action}
    </button>
  </div>
</Modal>
