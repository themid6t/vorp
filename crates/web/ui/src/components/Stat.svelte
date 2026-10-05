<script lang="ts">
  let { label, value, hint = '' }: { label: string; value: string | number; hint?: string } = $props();
</script>

<div class="card stat">
  <span class="label">{label}</span>
  <span class="value">{value}</span>
  {#if hint}<span class="dim small">{hint}</span>{/if}
</div>

<style>
  .stat { gap: 6px; padding: 22px 24px; }
  .label { font-size: 0.78rem; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; color: var(--text-dim); }
  .value { font-size: 1.9rem; font-weight: 650; letter-spacing: -0.03em; font-variant-numeric: tabular-nums; }
</style>
