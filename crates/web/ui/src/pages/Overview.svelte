<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type Tunnel } from '../lib/api';
  import { session } from '../lib/app.svelte';
  import { agentCommand, commandNote, formatRate, tunnelUrl } from '../lib/format';
  import CopyField from '../components/CopyField.svelte';
  import PageHead from '../components/PageHead.svelte';
  import Stat from '../components/Stat.svelte';

  const me = $derived(session.me!);
  const domain = $derived(session.config!.base_domain);
  let open = $state<number | null>(null);

  // Live state can be briefly unavailable; the overview still renders without it.
  onMount(() => {
    api<Tunnel[]>('/api/tunnels').then((t) => (open = t.length), () => {});
  });
</script>

<PageHead eyebrow="control plane" title="Overview">Your account, your quota, and how to open a tunnel.</PageHead>

<div class="grid-auto">
  <Stat label="Open tunnels" value={open ?? '—'} hint={me.limits ? `of ${me.limits.max_tunnels} allowed` : 'no limit as admin'} />
  {#if me.limits}
    <Stat label="Bandwidth" value={formatRate(me.limits.bandwidth_bytes_per_sec)} hint="up and down combined" />
    <Stat label="Concurrent requests" value={me.limits.max_concurrent_requests} hint="across all your tunnels" />
  {:else}
    <Stat label="Quota" value="Exempt" hint="administrators have no quota" />
  {/if}
</div>

<div class="split">
  <section class="card">
    <h2>Start a tunnel</h2>
    <ol>
      <li>Create a token on the <a href="#tokens">Tokens</a> page.</li>
      <li>Run the agent next to the app you want to share:</li>
    </ol>
    <CopyField label="Agent command" value={agentCommand(domain)} />
    {#if commandNote()}<p class="dim small">{commandNote()}</p>{/if}
  </section>

  <section class="card">
    <h2>Account</h2>
    <dl>
      <dt>Email</dt><dd>{me.email}</dd>
      <dt>Role</dt><dd>{me.is_admin ? 'Administrator' : 'User'}</dd>
      <dt>Subdomain</dt>
      <dd>
        {#if me.assigned_subdomain}
          <a href={tunnelUrl(me.assigned_subdomain, domain)} target="_blank" rel="noopener noreferrer">{tunnelUrl(me.assigned_subdomain, domain)}</a>
        {:else}
          <span class="dim">Assigned when you first need it.</span>
        {/if}
      </dd>
    </dl>
  </section>
</div>

<section class="card">
  <h2>How quotas behave</h2>
  <ul class="dim">
    <li>Over the bandwidth limit, transfers slow down. Nothing is dropped.</li>
    <li>Over the request limit, new requests wait up to 10 seconds, then visitors get a 503 with Retry-After.</li>
    <li>Over the tunnel limit, the agent's new tunnel is refused with <code>TUNNEL_LIMIT</code>.</li>
  </ul>
</section>

<style>
  .split { display: grid; gap: 24px; grid-template-columns: minmax(0, 3fr) minmax(0, 2fr); }
  @media (max-width: 1000px) { .split { grid-template-columns: 1fr; } }
  ol, ul { margin: 0; padding-left: 20px; display: grid; gap: 6px; }
  dl { margin: 0; display: grid; grid-template-columns: auto 1fr; gap: 10px 20px; font-size: 0.92rem; }
  dt { color: var(--text-dim); font-weight: 550; }
  dd { margin: 0; overflow-wrap: anywhere; }
</style>
