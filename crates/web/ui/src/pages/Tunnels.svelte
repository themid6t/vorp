<script lang="ts">
  import { onMount } from 'svelte';
  import { api, post, type Tunnel } from '../lib/api';
  import { attempt, confirm, notify, session } from '../lib/app.svelte';
  import { tunnelUrl } from '../lib/format';
  import PageHead from '../components/PageHead.svelte';

  const limits = $derived(session.me!.limits);
  const domain = $derived(session.config!.base_domain);
  let tunnels = $state<Tunnel[] | null>(null);
  let refreshing = $state(false);
  let closing = $state<string | null>(null);

  async function refresh() {
    refreshing = true;
    await attempt(async () => {
      tunnels = await api<Tunnel[]>('/api/tunnels', { errors: { 503: 'Live tunnel state is unavailable right now.' } });
    });
    refreshing = false;
  }

  onMount(() => {
    refresh();
    const timer = setInterval(() => { if (!document.hidden) refresh(); }, 5000);
    return () => clearInterval(timer);
  });

  async function close(name: string) {
    if (!(await confirm(`Close ${name}?`, 'Visitors lose access until the agent opens it again.', 'Close tunnel'))) return;
    closing = name;
    await attempt(async () => {
      const result = await post<{ ok: boolean }>(`/api/tunnels/${encodeURIComponent(name)}/close`);
      notify(result.ok ? `Closed ${name}.` : `${name} was already closed.`, true);
      await refresh();
    });
    closing = null;
  }

  const summary = $derived.by(() => {
    if (!tunnels) return 'Loading…';
    const n = tunnels.length;
    if (!limits) return `${n} tunnel${n === 1 ? '' : 's'} open. Administrators have no tunnel limit.`;
    return `${n} of ${limits.max_tunnels} tunnels in use.` +
      (n >= limits.max_tunnels ? " You are at your limit: the agent's next tunnel will be refused with TUNNEL_LIMIT." : '');
  });
</script>

<PageHead eyebrow="live" title="Tunnels">{summary}</PageHead>

<section class="card flush">
  <div class="bar">
    <h2>Open tunnels</h2>
    <span class="badge ok">auto-refresh</span>
    <button type="button" class="sm" onclick={refresh} disabled={refreshing}>Refresh</button>
  </div>
  {#if tunnels?.length}
    <table>
      <thead><tr><th>URL</th><th>Machine</th><th>Upstream</th><th>Active</th><th><span class="sr-only">Actions</span></th></tr></thead>
      <tbody>
        {#each tunnels as t (t.subdomain)}
          <tr>
            <td data-label="URL"><a href={tunnelUrl(t.subdomain, domain)} target="_blank" rel="noopener noreferrer">{tunnelUrl(t.subdomain, domain)}</a></td>
            <td data-label="Machine" class="mono">{t.machine_id}</td>
            <td data-label="Upstream" class="mono">{t.upstream_hint || '—'}</td>
            <td data-label="Active">{t.active_requests}</td>
            <td class="actions"><button type="button" class="danger sm" disabled={closing === t.subdomain} onclick={() => close(t.subdomain)}>Close</button></td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else if tunnels}
    <p class="empty">No tunnels are open. Start the agent to open one — the command is on the <a href="#overview">overview</a>.</p>
  {/if}
</section>

