<script lang="ts">
  import { onMount } from 'svelte';
  import { api, post, type Tunnel } from '../lib/api';
  import { attempt, confirm, notify, session } from '../lib/app.svelte';
  import { tunnelUrl } from '../lib/format';
  import Icon from '../components/Icon.svelte';
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

<section class="card flush tunnel-card">
  <div class="tunnel-head">
    <div>
      <h2>Open tunnels</h2>
      <p class="dim small">Manage and monitor active tunnels.</p>
    </div>
    <div class="head-actions">
      <span class="badge ok">Auto-refresh</span>
      <button type="button" class="refresh-button sm" onclick={refresh} disabled={refreshing}>
        <Icon name="refresh" size={14} />Refresh
      </button>
    </div>
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
    <div class="empty tunnel-empty">
      <Icon name="share" size={25} />
      <strong>No tunnels open</strong>
      <p>Start the agent to open a tunnel. The command is available on <a href="#overview">Overview</a>.</p>
    </div>
  {/if}
</section>

<style>
  .tunnel-card { gap: 0; }
  .tunnel-head {
    display: flex; align-items: center; justify-content: space-between; gap: 16px;
    padding: 14px 16px; border-bottom: 1px solid var(--color-border);
  }
  .tunnel-head > div:first-child { display: grid; gap: 5px; }
  .head-actions { display: flex; align-items: center; justify-content: flex-end; gap: 8px; }
  .refresh-button { color: var(--text); background: transparent; border-color: var(--color-border); }
  .refresh-button:hover:not(:disabled) { color: var(--text); background: var(--hover-surface); filter: none; }
  .tunnel-empty { min-height: 140px; display: grid; place-items: center; align-content: center; gap: 7px; }
  .tunnel-empty strong { color: var(--text); font-size: 0.92rem; }
  .tunnel-empty p { font-size: 0.8rem; }
  @media (max-width: 600px) {
    .tunnel-head { align-items: flex-start; flex-direction: column; }
    .head-actions { width: 100%; }
  }
</style>

