<script lang="ts">
  import { onMount } from 'svelte';
  import { api, post, type AdminUser, type LimitField, type Limits, type ReservationRequest } from '../lib/api';
  import { attempt, confirm, notify, session } from '../lib/app.svelte';
  import { bandwidthBytes, bytesToMib, formatRate, positiveInt } from '../lib/format';
  import Icon from '../components/Icon.svelte';
  import PageHead from '../components/PageHead.svelte';
  import LimitsDialog from './LimitsDialog.svelte';

  const UNPUSHED = 'Saved, but live tunnels have not picked it up yet. Save again to retry.';
  const me = $derived(session.me!);

  let defaults = $state<Limits | null>(null);
  let users = $state<AdminUser[] | null>(null);
  let requests = $state<ReservationRequest[] | null>(null);
  let editing = $state<AdminUser | null>(null);
  let busy = $state<string | null>(null);
  let form = $state({ max_tunnels: '' as number | string, bandwidth: '' as number | string, max_concurrent_requests: '' as number | string });
  let newUser = $state({ email: '', password: '' });

  async function load() {
    const [limits, u, r] = await Promise.all([
      api<Limits>('/api/admin/limits'), api<AdminUser[]>('/api/admin/users'), api<ReservationRequest[]>('/api/admin/reservation-requests')]);
    defaults = limits;
    users = u;
    requests = r;
    session.pendingRequests = r.length;
    form = { max_tunnels: limits.max_tunnels, bandwidth: bytesToMib(limits.bandwidth_bytes_per_sec), max_concurrent_requests: limits.max_concurrent_requests };
  }
  onMount(() => { attempt(load); });

  /** Runs one admin action with its control disabled, then reloads the page data. */
  async function act(key: string, action: () => Promise<void>) {
    busy = key;
    await attempt(action);
    busy = null;
  }

  async function decide(r: ReservationRequest, decision: 'approve' | 'reject') {
    if (decision === 'reject' && !(await confirm(`Reject ${r.name}?`, `${r.email}'s request is rejected and the name becomes free.`, 'Reject'))) return;
    await act(`req:${r.name}`, async () => {
      await post(`/api/admin/reservation-requests/${encodeURIComponent(r.name)}/${decision}`);
      notify(decision === 'approve' ? `${r.name} is now reserved for ${r.email}.` : `Rejected the request for ${r.name}.`, true);
      await load();
    });
  }

  // The PUT replaces both fields, so start from the user's current values.
  async function setAccess(u: AdminUser, change: { is_admin?: boolean; can_reserve_directly?: boolean }) {
    if (change.is_admin !== undefined && !(await confirm(
      change.is_admin ? `Make ${u.email} an administrator?` : `Remove ${u.email}'s admin role?`,
      change.is_admin ? 'Admins manage every user and have no quota.' : 'Their quota applies again at once.',
      change.is_admin ? 'Make admin' : 'Remove admin', !change.is_admin))) return;
    await act(`user:${u.id}`, async () => {
      try {
        await api(`/api/admin/users/${u.id}`, { method: 'PUT', body: { is_admin: u.is_admin, can_reserve_directly: u.can_reserve_directly, ...change }, errors: { 503: UNPUSHED } });
        notify(`Updated ${u.email}.`, true);
      } finally {
        // Reload either way so a failed toggle shows the stored value again.
        await load();
      }
    });
  }

  async function saveDefaults(event: SubmitEvent) {
    event.preventDefault();
    const body = {
      max_tunnels: positiveInt(form.max_tunnels),
      bandwidth_bytes_per_sec: bandwidthBytes(form.bandwidth),
      max_concurrent_requests: positiveInt(form.max_concurrent_requests),
    };
    if (body.bandwidth_bytes_per_sec === null) return notify('Bandwidth must be more than 0 MiB/s.');
    if (body.max_tunnels === null || body.max_concurrent_requests === null) return notify('Tunnels and requests must be whole numbers of at least 1.');
    await act('defaults', async () => {
      await api('/api/admin/limits', { method: 'PUT', body, errors: { 503: UNPUSHED } });
      notify('Default quotas saved.', true);
      await load();
    });
  }

  async function createUser(event: SubmitEvent) {
    event.preventDefault();
    await act('create', async () => {
      const user = await post<{ email: string }>('/api/users', newUser, { 409: 'That email is already registered.' });
      newUser = { email: '', password: '' };
      notify(`Created ${user.email}. Send them the password privately; they must change it at first login.`, true);
      await load();
    });
  }

  const QUOTA_COLUMNS: { field: LimitField; label: string }[] = [
    { field: 'max_tunnels', label: 'Tunnels' },
    { field: 'bandwidth_bytes_per_sec', label: 'Bandwidth' },
    { field: 'max_concurrent_requests', label: 'Requests' },
  ];
  const quota = (u: AdminUser, field: LimitField) =>
    u.effective ? (field === 'bandwidth_bytes_per_sec' ? formatRate(u.effective[field]) : String(u.effective[field])) : null;
</script>

<PageHead eyebrow="administration" title="Users & policy">
  Approve names, set quotas and manage accounts. Changes reach live tunnels immediately.
</PageHead>

<div class="admin-summary">
  <div class="summary-card"><span class="summary-icon users"><Icon name="account" /></span><span><small>Total users</small><strong>{users?.length ?? '—'}</strong></span></div>
  <div class="summary-card"><span class="summary-icon pending"><Icon name="clock" /></span><span><small>Pending requests</small><strong>{requests?.length ?? '—'}</strong></span></div>
  <div class="summary-card"><span class="summary-icon quota-icon"><Icon name="database" /></span><span><small>Default quota</small><strong>{defaults ? `${defaults.max_tunnels} / ${bytesToMib(defaults.bandwidth_bytes_per_sec)} / ${defaults.max_concurrent_requests}` : '—'}</strong><em>Tunnels / MiB/s / Requests</em></span></div>
  <div class="summary-card"><span class="summary-icon admins"><Icon name="admin" /></span><span><small>Administrators</small><strong>{users ? users.filter((user) => user.is_admin).length : '—'}</strong></span></div>
</div>

<section class="card flush requests-card">
  <div class="section-head">
    <span class="section-icon"><Icon name="clock" size={18} /></span>
    <div><h2>Reservation requests</h2><p class="dim small">Users without direct reservation ask for names here. A pending request holds the name until you decide.</p></div>
    {#if requests?.length}<span class="badge accent">{requests.length} pending</span>{/if}
  </div>
  {#if requests?.length}
    <table>
      <thead><tr><th>Name</th><th>Requested by</th><th>When</th><th><span class="sr-only">Actions</span></th></tr></thead>
      <tbody>
        {#each requests as r (r.name)}
          <tr>
            <td data-label="Name" class="mono">{r.name}</td>
            <td data-label="Requested by">{r.email}</td>
            <td data-label="When">{new Date(r.created_at_ms).toLocaleString()}</td>
            <td class="actions">
              <button type="button" class="primary sm" disabled={busy === `req:${r.name}`} onclick={() => decide(r, 'approve')}>Approve</button>
              <button type="button" class="danger sm" disabled={busy === `req:${r.name}`} onclick={() => decide(r, 'reject')}>Reject</button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else if requests}
    <div class="empty request-empty">
      <span class="empty-icon" aria-hidden="true"><Icon name="document" size={22} /></span>
      <strong>No pending requests.</strong>
      <p>Requests from users will appear here.</p>
    </div>
  {/if}
</section>

<section class="card flush users-card">
  <div class="section-head users-head">
    <Icon name="account" size={18} />
    <div><h2>Users</h2><p class="dim small"><span class="badge plain">default</span> marks a value that follows the default quota. Admins are exempt from quotas. "Direct" users get reserved names at once, without a request.</p></div>
  </div>
  {#if users}
    <table>
      <thead><tr><th>Email</th><th>Role</th><th>Direct</th><th>Tunnels</th><th>Bandwidth</th><th>Requests</th><th><span class="sr-only">Actions</span></th></tr></thead>
      <tbody>
        {#each users as u (u.id)}
          <tr>
            <td data-label="Email">
              {u.email}
              {#if u.must_change_password}<br><span class="badge warn">must set password</span>{/if}
            </td>
            <td data-label="Role">
              <span class="badge {u.is_admin ? 'accent' : ''}">{u.is_admin ? 'admin' : 'user'}</span>
              {#if u.id === me.id}<span class="dim small">(you)</span>{/if}
            </td>
            <td data-label="Direct">
              {#if u.is_admin}
                <span class="dim small">always</span>
              {:else}
                <input type="checkbox" aria-label="Direct reservation for {u.email}" checked={u.can_reserve_directly}
                  disabled={busy === `user:${u.id}`} onchange={(e) => setAccess(u, { can_reserve_directly: e.currentTarget.checked })}>
              {/if}
            </td>
            {#each QUOTA_COLUMNS as { field, label } (field)}
              <td data-label={label}>
                {#if quota(u, field) === null}
                  <span class="dim small">exempt</span>
                {:else}
                  {quota(u, field)}
                  {#if u.overrides[field] == null}<span class="badge plain">default</span>{/if}
                {/if}
              </td>
            {/each}
            <td class="actions">
              {#if u.id !== me.id}
                <button type="button" class="sm" disabled={busy === `user:${u.id}`} onclick={() => setAccess(u, { is_admin: !u.is_admin })}>
                  {u.is_admin ? 'Make user' : 'Make admin'}
                </button>
              {/if}
              {#if !u.is_admin}<button type="button" class="sm" onclick={() => (editing = u)}>Edit quota</button>{/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</section>

<div class="split">
  <form class="card" onsubmit={saveDefaults}>
    <div class="form-heading"><Icon name="database" size={19} /><h2>Default quotas</h2></div>
    <p class="dim small">For every non-admin user without an override. Lowering the tunnel limit does not close tunnels that are already open.</p>
    <div class="fields">
      <label>Max tunnels <input type="number" min="1" step="1" bind:value={form.max_tunnels} required></label>
      <label>Bandwidth (MiB/s) <input type="number" min="0.001" step="any" bind:value={form.bandwidth} required></label>
      <label>Concurrent requests <input type="number" min="1" step="1" bind:value={form.max_concurrent_requests} required></label>
    </div>
    <div class="row end"><button type="submit" class="primary" disabled={busy === 'defaults'}>Save defaults</button></div>
  </form>

  <form class="card" onsubmit={createUser}>
    <div class="form-heading"><Icon name="userPlus" size={19} /><h2>Create a user</h2></div>
    <p class="dim small">Send the password over a private channel. The user must choose a new one at first login.</p>
    <label>Email <input type="email" autocomplete="off" bind:value={newUser.email} required></label>
    <label>Password <input type="password" autocomplete="new-password" minlength="12" bind:value={newUser.password} required></label>
    <div class="row end"><button type="submit" class="primary" disabled={busy === 'create'}>Create user</button></div>
  </form>
</div>

{#if defaults}
  <LimitsDialog bind:user={editing} {defaults} saved={load} unpushed={UNPUSHED} />
{/if}

<style>
  .admin-summary { display: grid; gap: 12px; grid-template-columns: repeat(4, minmax(0, 1fr)); }
  .summary-card { min-width: 0; min-height: 80px; padding: 12px 14px; display: flex; align-items: flex-start; gap: 10px; border: 1px solid var(--card-border); border-radius: var(--radius); background: var(--color-surface); box-shadow: var(--card-shadow); }
  .summary-card > span:last-child { min-width: 0; display: grid; gap: 1px; }
  .summary-card small { color: var(--text-dim); font-size: 0.62rem; font-style: normal; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; }
  .summary-card strong { font-size: 1.35rem; line-height: 1.2; white-space: nowrap; }
  .summary-card em { color: var(--text-dim); font-size: 0.62rem; font-style: normal; white-space: nowrap; }
  .summary-icon { width: 30px; height: 30px; display: grid; place-items: center; flex: none; border-radius: 8px; }
  .summary-icon.users { color: var(--accent-ink); background: var(--accent-soft); }
  .summary-icon.pending { color: var(--ok); background: var(--teal-surface); }
  .summary-icon.quota-icon { color: var(--warn); background: var(--warm-highlight); }
  .summary-icon.admins { color: var(--text-dim); background: var(--active-surface); }
  .requests-card, .users-card { gap: 0; }
  .section-head { display: flex; align-items: flex-start; gap: 10px; padding: 12px 16px; }
  .section-head > div { min-width: 0; display: grid; gap: 5px; }
  .section-head > .badge { margin-left: auto; }
  .section-icon { width: 32px; height: 32px; display: grid; place-items: center; flex: none; border-radius: 50%; color: var(--text-dim); background: var(--active-surface); }
  .users-head { align-items: center; border-bottom: 1px solid var(--color-border); }
  .users-head > :global(svg) { flex: none; }
  .request-empty { min-height: 100px; display: grid; place-items: center; align-content: center; gap: 5px; }
  .empty-icon { width: 42px; height: 42px; margin-bottom: 2px; display: grid; place-items: center; border-radius: 50%; color: var(--text-dim); background: var(--active-surface); }
  .request-empty strong { color: var(--text); font-size: 0.82rem; }
  .request-empty p { font-size: 0.72rem; }
  .split { display: grid; gap: 14px; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); align-items: stretch; }
  .split form { height: 100%; }
  .split .row.end { margin-top: auto; }
  .form-heading { display: flex; align-items: center; gap: 9px; }
  .fields { display: grid; gap: 14px; grid-template-columns: repeat(auto-fit, minmax(140px, 1fr)); }
  td .badge.plain { margin-left: 6px; }
  @media (max-width: 980px) { .admin-summary { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
  @media (max-width: 560px) { .admin-summary { grid-template-columns: minmax(0, 1fr); } }
</style>
