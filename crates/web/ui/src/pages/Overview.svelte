<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type Limits, type Reservation, type Token, type TrafficEvent, type Tunnel } from '../lib/api';
  import { session } from '../lib/app.svelte';
  import { agentCommand, commandNote, formatBytes, formatRate } from '../lib/format';
  import CopyField from '../components/CopyField.svelte';
  import Icon from '../components/Icon.svelte';
  import PageHead from '../components/PageHead.svelte';

  const me = $derived(session.me!);
  const domain = $derived(session.config!.base_domain);
  let tunnels = $state<Tunnel[] | null>(null);
  let tokens = $state<Token[] | null>(null);
  let reservations = $state<Reservation[] | null>(null);
  let traffic = $state<TrafficEvent[] | null>(null);
  let adminLimits = $state<Limits | null>(null);
  const activeTokens = $derived(tokens?.filter((token) => !token.revoked).length ?? null);
  const reservedNames = $derived(reservations?.filter((reservation) => reservation.status === 'reserved').length ?? null);
  const recentTraffic = $derived(traffic ? [...traffic].sort((a, b) => b.timestamp_ms - a.timestamp_ms).slice(0, 5) : null);
  const resourceLimits = $derived(me.limits ?? adminLimits);

  // Each summary remains useful when another live endpoint is briefly unavailable.
  onMount(() => {
    api<Tunnel[]>('/api/tunnels').then((value) => (tunnels = value), () => {});
    api<Token[]>('/api/tokens').then((value) => (tokens = value), () => {});
    api<Reservation[]>('/api/reservations').then((value) => (reservations = value), () => {});
    api<TrafficEvent[]>('/api/traffic/recent').then((value) => (traffic = value), () => {});
    if (me.is_admin) api<Limits>('/api/admin/limits').then((value) => (adminLimits = value), () => {});
  });
</script>

<PageHead eyebrow="control plane" title="Overview">Your tunnels, quota, and connection status at a glance.</PageHead>

<div class="summary-grid">
  <div class="summary-card"><span class="summary-icon tunnels"><Icon name="tunnels" /></span><span><small>Open tunnels</small><strong>{tunnels?.length ?? '—'}</strong><em>{me.limits ? `of ${me.limits.max_tunnels} allowed` : 'no limit as admin'}</em></span></div>
  <div class="summary-card"><span class="summary-icon quota"><Icon name="database" /></span><span><small>Quota</small><strong>{me.limits ? `${me.limits.max_tunnels} tunnels` : 'Exempt'}</strong><em>Account quota status</em></span></div>
  <div class="summary-card"><span class="summary-icon tokens"><Icon name="tokens" /></span><span><small>Active tokens</small><strong>{activeTokens ?? '—'}</strong><em>Active authentication tokens</em></span></div>
  <div class="summary-card"><span class="summary-icon names"><Icon name="names" /></span><span><small>Reserved names</small><strong>{reservedNames ?? '—'}</strong><em>Reserved subdomains</em></span></div>
</div>

{#if resourceLimits}
  <section class="card limits-card">
    <h2>Resource limits</h2>
    <div class="limits-grid">
      <div class="limit"><span class="limit-icon"><Icon name="traffic" /></span><span><small>Bandwidth</small><strong>{formatRate(resourceLimits.bandwidth_bytes_per_sec)}</strong><em>up and down combined</em></span></div>
      <div class="limit"><span class="limit-icon"><Icon name="account" /></span><span><small>Concurrent requests</small><strong>{resourceLimits.max_concurrent_requests}</strong><em>across all your tunnels</em></span></div>
    </div>
  </section>
{/if}

<div class="overview-split">
  <section class="card overview-card start-card">
    <h2>Start a tunnel</h2>
    <ol>
      <li>Create a token on the <a href="#tokens">Tokens</a> page.</li>
      <li>Run the agent next to the app you want to share:</li>
    </ol>
    <CopyField label="Agent command" value={agentCommand(domain)} />
    {#if commandNote()}<p class="dim small">{commandNote()}</p>{/if}
  </section>

  <section class="card overview-card connection-card">
    <h2>Connection status</h2>
    <p class="connected"><span></span>Relay connected</p>
    <p class="dim small">This dashboard is connected to your configured relay.</p>
    <dl>
      <dt>Relay address</dt><dd class="mono">vorp.{domain}</dd>
      <dt>Session</dt><dd>Authenticated</dd>
      <dt>Account</dt><dd>{me.is_admin ? 'Administrator' : 'User'}</dd>
    </dl>
  </section>
</div>

<section class="card flush data-card">
  <div class="data-head"><h2>Recent tunnels</h2><a href="#tunnels">View all</a></div>
  {#if tunnels?.length}
    <table>
      <thead><tr><th>Subdomain</th><th>Upstream</th><th>Status</th><th>Active requests</th></tr></thead>
      <tbody>
        {#each tunnels.slice(0, 5) as tunnel (tunnel.subdomain)}
          <tr><td data-label="Subdomain" class="mono">{tunnel.subdomain}.{domain}</td><td data-label="Upstream" class="mono">{tunnel.upstream_hint || '—'}</td><td data-label="Status"><span class="badge ok">Active</span></td><td data-label="Active requests">{tunnel.active_requests}</td></tr>
        {/each}
      </tbody>
    </table>
  {:else if tunnels}
    <div class="compact-empty"><Icon name="tunnels" size={22} /><strong>No tunnels open</strong><span>Start the agent using the command above.</span></div>
  {/if}
</section>

<section class="card flush data-card">
  <div class="data-head"><h2>Recent traffic</h2><a href="#traffic">View all</a></div>
  {#if recentTraffic?.length}
    <table>
      <thead><tr><th>Time</th><th>Tunnel</th><th>Method</th><th>Status</th><th>Transferred</th></tr></thead>
      <tbody>
        {#each recentTraffic as event, index (index)}
          <tr><td data-label="Time">{new Date(event.timestamp_ms).toLocaleTimeString()}</td><td data-label="Tunnel" class="mono">{event.subdomain}</td><td data-label="Method">{event.method}</td><td data-label="Status">{event.status}</td><td data-label="Transferred">{formatBytes(event.bytes_in + event.bytes_out)}</td></tr>
        {/each}
      </tbody>
    </table>
  {:else if recentTraffic}
    <div class="compact-empty"><Icon name="traffic" size={23} /><span>Requests through your tunnels will appear here.</span></div>
  {/if}
</section>

<style>
  .summary-grid { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 12px; }
  .summary-card { min-width: 0; min-height: 82px; padding: 12px 14px; display: flex; align-items: flex-start; gap: 11px; border: 1px solid var(--card-border); border-radius: var(--radius); background: var(--color-surface); box-shadow: var(--card-shadow); }
  .summary-card > span:last-child { min-width: 0; display: grid; gap: 2px; }
  .summary-card small { color: var(--text-dim); font-size: 0.65rem; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; }
  .summary-card strong { overflow: hidden; font-size: 1.35rem; line-height: 1.15; text-overflow: ellipsis; white-space: nowrap; }
  .summary-card em, .limit em { overflow: hidden; color: var(--text-dim); font-size: 0.65rem; font-style: normal; text-overflow: ellipsis; white-space: nowrap; }
  .summary-icon { width: 32px; height: 32px; display: grid; place-items: center; flex: none; border-radius: 8px; color: var(--text-dim); background: var(--active-surface); }
  .summary-icon.tunnels { color: var(--accent-ink); background: var(--accent-soft); }
  .summary-icon.quota { color: var(--accent-ink); background: var(--accent-soft); }
  .summary-icon.tokens { color: var(--warn); background: var(--warm-highlight); }
  .summary-icon.names { color: var(--accent-ink); background: var(--accent-soft); }
  .limits-card { padding: 13px 16px; gap: 10px; }
  .limits-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .limit { min-width: 0; display: flex; align-items: center; gap: 12px; padding: 0 8px; }
  .limit + .limit { border-left: 1px solid var(--color-border); padding-left: 38px; }
  .limit > span:last-child { min-width: 0; display: grid; gap: 2px; }
  .limit small { color: var(--text-dim); font-size: 0.65rem; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; }
  .limit strong { font-size: 1.2rem; }
  .limit-icon { width: 34px; height: 34px; display: grid; place-items: center; flex: none; border-radius: 50%; color: var(--text-dim); background: var(--active-surface); }
  .overview-split { display: grid; grid-template-columns: minmax(0, 1.7fr) minmax(260px, 1fr); gap: 14px; align-items: stretch; }
  .overview-card { min-width: 0; padding: 14px 16px; gap: 10px; }
  ol { margin: 0; padding-left: 18px; display: grid; gap: 3px; }
  .connected { display: flex; align-items: center; gap: 8px; color: var(--ok); font-weight: 600; }
  .connected span { width: 9px; height: 9px; border-radius: 50%; background: currentColor; }
  dl { margin: 3px 0 0; display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 0 12px; font-size: 0.75rem; }
  dt, dd { padding: 7px 0; border-bottom: 1px solid var(--color-border); }
  dt { color: var(--text-dim); }
  dd { margin: 0; overflow-wrap: anywhere; text-align: right; }
  .data-card { gap: 0; }
  .data-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 12px 16px; border-bottom: 1px solid var(--color-border); }
  .data-head a { font-size: 0.75rem; }
  .compact-empty { min-height: 112px; display: flex; align-items: center; justify-content: center; flex-direction: column; gap: 7px; padding: 20px; color: var(--text-dim); font-size: 0.8rem; text-align: center; }
  .compact-empty strong { color: var(--text); }
  @media (max-width: 980px) { .summary-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
  @media (max-width: 760px) { .overview-split { grid-template-columns: minmax(0, 1fr); } }
  @media (max-width: 520px) {
    .summary-grid { grid-template-columns: minmax(0, 1fr); }
    .limits-grid { grid-template-columns: minmax(0, 1fr); gap: 12px; }
    .limit + .limit { border-top: 1px solid var(--color-border); border-left: 0; padding: 12px 8px 0; }
    .overview-card { padding: 13px 14px; }
  }
</style>
