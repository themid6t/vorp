<script lang="ts">
  import { api, type AdminUser, type LimitField, type Limits } from '../lib/api';
  import { attempt, notify } from '../lib/app.svelte';
  import { bandwidthBytes, bytesToMib, formatRate, positiveInt } from '../lib/format';
  import Modal from '../components/Modal.svelte';

  let { user = $bindable(), defaults, saved, unpushed }: {
    user: AdminUser | null; defaults: Limits; saved: () => Promise<void>; unpushed: string;
  } = $props();

  const FIELDS: { field: LimitField; label: string }[] = [
    { field: 'max_tunnels', label: 'Max tunnels' },
    { field: 'bandwidth_bytes_per_sec', label: 'Bandwidth (MiB/s)' },
    { field: 'max_concurrent_requests', label: 'Concurrent requests' },
  ];
  const isRate = (f: LimitField) => f === 'bandwidth_bytes_per_sec';
  const show = (f: LimitField, v: number) => (isRate(f) ? bytesToMib(v) : v);

  // One editable row per field: `inherit` means "follow the default".
  let rows = $state<Record<LimitField, { inherit: boolean; value: number | string }>>({} as never);
  let busy = $state(false);

  $effect(() => {
    if (!user) return;
    const next = {} as typeof rows;
    for (const { field } of FIELDS) {
      const override = user.overrides[field];
      next[field] = { inherit: override == null, value: show(field, override ?? defaults[field]) };
    }
    rows = next;
  });

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (!user) return;
    // The PUT replaces every field, so always send all three: null means "use the default".
    const body: Record<string, number | null> = {};
    for (const { field, label } of FIELDS) {
      const row = rows[field];
      if (row.inherit) {
        body[field] = null;
        continue;
      }
      const value = isRate(field) ? bandwidthBytes(row.value) : positiveInt(row.value);
      if (value === null) {
        notify(isRate(field) ? 'Bandwidth must be more than 0 MiB/s.' : `${label} must be a whole number of at least 1.`);
        return;
      }
      body[field] = value;
    }
    const target = user;
    busy = true;
    await attempt(async () => {
      await api(`/api/admin/users/${target.id}/limits`, { method: 'PUT', body, errors: { 503: unpushed } });
      user = null;
      notify(`Quota for ${target.email} saved.`, true);
      await saved();
    });
    busy = false;
  }
</script>

<Modal bind:open={() => user !== null, (v) => { if (!v) user = null; }} title={user?.email ?? ''} eyebrow="quota">
  <form onsubmit={save}>
    <p class="dim small">Tick "default" to follow the default quota. Lowering the tunnel limit does not close tunnels that are already open.</p>
    {#each FIELDS as { field, label } (field)}
      {#if rows[field]}
        <div class="override">
          <label>{label}
            <input type="number" min={isRate(field) ? 0.001 : 1} step={isRate(field) ? 'any' : 1} bind:value={rows[field].value} disabled={rows[field].inherit}>
          </label>
          <label class="choice"><input type="checkbox" bind:checked={rows[field].inherit}> default ({isRate(field) ? formatRate(defaults[field]) : defaults[field]})</label>
        </div>
      {/if}
    {/each}
    <div class="row end">
      <button type="button" onclick={() => (user = null)}>Cancel</button>
      <button type="submit" class="primary" disabled={busy}>Save</button>
    </div>
  </form>
</Modal>

<style>
  form { display: grid; gap: 16px; }
  .override { display: grid; grid-template-columns: minmax(0, 1fr) 190px; gap: 12px; align-items: end; }
  .choice { padding: 10px 12px; align-items: center; white-space: nowrap; font-size: 0.82rem; }
  @media (max-width: 500px) { .override { grid-template-columns: 1fr; } }
</style>
