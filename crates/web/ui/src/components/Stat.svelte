<script lang="ts">
  let { label, value, hint = '' }: { label: string; value: string | number; hint?: string } = $props();
</script>

<div class="card stat">
  <span class="label">{label}</span>
  <span class="value">{value}</span>
  {#if hint}<span class="dim small">{hint}</span>{/if}
</div>

<style>
  .stat { height: 100%; min-height: 82px; gap: 2px; padding: 12px 16px; align-content: center; }
  .label { font-size: 0.68rem; font-weight: 500; letter-spacing: 0.05em; text-transform: uppercase; color: var(--text-dim); }
  .value { font-size: clamp(1.4rem, 2.2vw, 1.5rem); line-height: 1.1; font-weight: 600; letter-spacing: -0.03em; font-variant-numeric: tabular-nums; }
</style>
