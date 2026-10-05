<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type Reservation } from '../lib/api';
  import { attempt, confirm, notify, session } from '../lib/app.svelte';
  import { tunnelUrl } from '../lib/format';
  import PageHead from '../components/PageHead.svelte';

  const direct = $derived(session.me!.can_reserve_directly);
  const domain = $derived(session.config!.base_domain);
  let names = $state<Reservation[] | null>(null);
  let name = $state('');
  let busy = $state(false);
  let releasing = $state<string | null>(null);

  async function load() { names = await api<Reservation[]>('/api/reservations'); }
  onMount(() => { attempt(load); });

  async function reserve(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    await attempt(async () => {
      // A 409 carries the relay's explanation of who holds the name.
      const result = await api<Reservation>('/api/reservations', { method: 'POST', body: { name: name.trim().toLowerCase() } });
      name = '';
      notify(result.status === 'reserved'
        ? `Reserved ${result.name}.`
        : `Requested ${result.name}. It is yours once an administrator approves it.`, true);
      await load();
    });
    busy = false;
  }

  async function release(r: Reservation) {
    const isReserved = r.status === 'reserved';
    const ok = isReserved
      ? await confirm(`Release ${r.name}?`, 'Anyone can reserve it afterwards.', 'Release')
      : await confirm(`Withdraw ${r.name}?`, 'Your request is cancelled and the name becomes free.', 'Withdraw');
    if (!ok) return;
    releasing = r.name;
    await attempt(async () => {
      await api(`/api/reservations/${encodeURIComponent(r.name)}`, { method: 'DELETE' });
      notify(isReserved ? `Released ${r.name}.` : `Withdrew the request for ${r.name}.`, true);
      await load();
    });
    releasing = null;
  }
</script>

<PageHead eyebrow="names" title="Reserved subdomains">
  Only your tokens can open a name you have reserved. Your tunnels appear at <code>{tunnelUrl('<name>', domain)}</code>.
</PageHead>

<form class="card" onsubmit={reserve}>
  <h2>{direct ? 'Reserve a name' : 'Request a name'}</h2>
  <p class="dim small">
    {direct
      ? 'Names you reserve are yours at once.'
      : 'An administrator approves each name you request. A pending request already holds the name, so nobody else can take it.'}
  </p>
  <div class="row">
    <div class="field grow">
      <input bind:value={name} aria-label="Subdomain" placeholder="my-app" autocomplete="off" spellcheck="false" required>
      <span class="suffix mono">.{domain}</span>
    </div>
    <button type="submit" class="primary" disabled={busy}>{direct ? 'Reserve' : 'Request'}</button>
  </div>
</form>

<section class="card flush">
  <div class="bar"><h2>Your names</h2></div>
  {#if names?.length}
    <table>
      <thead><tr><th>Name</th><th>Status</th><th>URL</th><th><span class="sr-only">Actions</span></th></tr></thead>
      <tbody>
        {#each names as r (r.name)}
          <tr>
            <td data-label="Name" class="mono">{r.name}</td>
            <td data-label="Status">
              {#if r.status === 'reserved'}<span class="badge ok">reserved</span>{:else}<span class="badge warn">awaiting approval</span>{/if}
            </td>
            <td data-label="URL">
              {#if r.status === 'reserved'}<a href={tunnelUrl(r.name, domain)} target="_blank" rel="noopener noreferrer">{tunnelUrl(r.name, domain)}</a>{:else}—{/if}
            </td>
            <td class="actions">
              <button type="button" class="danger sm" disabled={releasing === r.name} onclick={() => release(r)}>
                {r.status === 'reserved' ? 'Release' : 'Withdraw'}
              </button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else if names}
    <p class="empty">You have not reserved or requested any names.</p>
  {/if}
</section>

<style>
  .field { display: flex; align-items: center; gap: 10px; }
  .suffix { color: var(--text-dim); font-size: 0.85rem; white-space: nowrap; }
  @media (max-width: 600px) { .suffix { display: none; } }
</style>
