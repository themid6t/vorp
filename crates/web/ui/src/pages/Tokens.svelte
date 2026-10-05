<script lang="ts">
  import { onMount } from 'svelte';
  import { api, post, type BindPolicy, type Reservation, type Token } from '../lib/api';
  import { attempt, confirm, notify, session } from '../lib/app.svelte';
  import { agentCommand, commandNote } from '../lib/format';
  import CopyField from '../components/CopyField.svelte';
  import Modal from '../components/Modal.svelte';
  import PageHead from '../components/PageHead.svelte';

  const POLICIES: { value: BindPolicy; title: string; text: string }[] = [
    { value: 'any', title: 'Any', text: 'Random names, your assigned subdomain, and any name you have reserved.' },
    { value: 'temporary', title: 'Temporary', text: 'Random names only.' },
    { value: 'reserved', title: 'Reserved', text: 'Only the reserved names you pick below.' },
  ];

  let tokens = $state<Token[] | null>(null);
  let reserved = $state<string[]>([]);
  let policy = $state<BindPolicy>('any');
  let allowlist = $state<string[]>([]);
  let creating = $state(false);
  let revoking = $state<number | null>(null);
  // The raw value exists only in this variable until the modal closes.
  let created = $state<{ raw: string; names: string[] } | null>(null);
  let revealOpen = $state(false);

  async function load() {
    const [t, r] = await Promise.all([api<Token[]>('/api/tokens'), api<Reservation[]>('/api/reservations')]);
    tokens = t;
    reserved = r.filter((x) => x.status === 'reserved').map((x) => x.name);
  }
  onMount(() => { attempt(load); });

  async function create(event: SubmitEvent) {
    event.preventDefault();
    const names = policy === 'reserved' ? allowlist : [];
    if (policy === 'reserved' && !names.length) {
      notify('Pick at least one reserved name for a reserved token.');
      return;
    }
    creating = true;
    await attempt(async () => {
      const body = names.length ? { bind_policy: policy, allowlist: names } : { bind_policy: policy };
      const result = await post<{ raw_token: string }>('/api/tokens', body);
      created = { raw: result.raw_token, names };
      revealOpen = true;
      policy = 'any';
      allowlist = [];
      await load();
    });
    creating = false;
  }

  $effect(() => { if (!revealOpen) created = null; });

  async function revoke(id: number) {
    if (!(await confirm(`Revoke token ${id}?`, 'Agents using it are disconnected now and cannot reconnect.', 'Revoke'))) return;
    revoking = id;
    await attempt(async () => {
      await post(`/api/tokens/${id}/revoke`, undefined,
        { 503: 'The token is revoked, but live agents could not be disconnected yet. Revoke again to retry.' });
      notify(`Token ${id} revoked and its agents disconnected.`, true);
      await load();
    });
    revoking = null;
  }
</script>

<PageHead eyebrow="credentials" title="Agent tokens">
  A token lets an agent open tunnels for you. Revoking one also disconnects every agent using it.
</PageHead>

<form class="card" onsubmit={create}>
  <h2>New token</h2>
  <fieldset>
    <legend>The token may open</legend>
    <div class="policies">
      {#each POLICIES as p (p.value)}
        <label class="choice">
          <input type="radio" name="policy" value={p.value} bind:group={policy}>
          <span><strong>{p.title}</strong><br><span class="dim small">{p.text}</span></span>
        </label>
      {/each}
    </div>
  </fieldset>
  {#if policy === 'reserved'}
    <fieldset>
      <legend>Allowed reserved names</legend>
      {#if reserved.length}
        <div class="row">
          {#each reserved as name (name)}
            <label class="choice name"><input type="checkbox" value={name} bind:group={allowlist}> <span class="mono">{name}</span></label>
          {/each}
        </div>
      {:else}
        <p class="dim">You have no approved reserved names. Reserve one on the <a href="#names">Subdomains</a> page first.</p>
      {/if}
    </fieldset>
  {/if}
  <div><button type="submit" class="primary" disabled={creating}>Create token</button></div>
</form>

<section class="card flush">
  <div class="bar"><h2>Your tokens</h2></div>
  {#if tokens?.length}
    <table>
      <thead><tr><th>ID</th><th>May open</th><th>Allowed names</th><th>Status</th><th><span class="sr-only">Actions</span></th></tr></thead>
      <tbody>
        {#each tokens as t (t.id)}
          <tr>
            <td data-label="ID" class="mono">#{t.id}</td>
            <td data-label="May open">{t.bind_policy}</td>
            <td data-label="Allowed names" class="mono">{t.allowlist.join(', ') || '—'}</td>
            <td data-label="Status">
              {#if t.revoked}<span class="badge bad">revoked</span>{:else}<span class="badge ok">active</span>{/if}
            </td>
            <td class="actions">
              {#if !t.revoked}<button type="button" class="danger sm" disabled={revoking === t.id} onclick={() => revoke(t.id)}>Revoke</button>{/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else if tokens}
    <p class="empty">You have no tokens yet.</p>
  {/if}
</section>

<Modal bind:open={revealOpen} dismissible={false} title="Copy your token now" eyebrow="token created">
  {#if created}
    <p class="warn"><strong>This is the only time this token is shown.</strong> Neither you nor the relay can view it again. If you lose it, revoke it and create a new one.</p>
    <CopyField label="New agent token" value={created.raw} />
    <p class="dim small">Run the agent with it:</p>
    <CopyField label="Agent command" value={agentCommand(session.config!.base_domain, created.raw, created.names)} />
    {#if commandNote()}<p class="dim small">{commandNote()}</p>{/if}
    <div class="row end"><button type="button" class="primary" onclick={() => (revealOpen = false)}>I've saved it</button></div>
  {/if}
</Modal>

<style>
  .policies { display: grid; gap: 14px; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); }
  .choice.name { padding: 10px 14px; align-items: center; }
  .warn { font-size: 0.92rem; padding: 12px 14px; border-radius: 12px; box-shadow: var(--inset-sm); }
  .warn strong { color: var(--warn); }
</style>
