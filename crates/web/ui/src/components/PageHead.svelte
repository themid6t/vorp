<script lang="ts">
  import type { Snippet } from 'svelte';
  let { eyebrow, title, children }: { eyebrow: string; title: string; children?: Snippet } = $props();
</script>

<header>
  <p class="eyebrow">{eyebrow}</p>
  <h1>{title}</h1>
  {#if children}<p class="sub">{@render children()}</p>{/if}
</header>

<style>
  header { display: grid; gap: 8px; margin-bottom: 4px; }
  .eyebrow { font-size: 0.75rem; font-weight: 600; letter-spacing: 0.1em; text-transform: uppercase; color: var(--accent-ink); }
  .sub { color: var(--text-dim); max-width: 64ch; }
</style>
