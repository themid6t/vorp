<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type TrafficEvent } from '../lib/api';
  import { attempt } from '../lib/app.svelte';
  import { formatBytes } from '../lib/format';
  import Icon from '../components/Icon.svelte';
  import PageHead from '../components/PageHead.svelte';

  type Status = { text: string; tone: 'ok' | 'warn' | 'bad' };
  let events = $state<TrafficEvent[] | null>(null);
  let status = $state<Status>({ text: 'Connecting', tone: 'warn' });
  let ended = $state(false);
  let feed: EventSource | null = null;

  const sorted = $derived(events ? [...events].sort((a, b) => b.timestamp_ms - a.timestamp_ms) : null);

  function openFeed() {
    ended = false;
    status = { text: 'Connecting', tone: 'warn' };
    const source = new EventSource('/api/traffic/stream');
    feed = source;
    source.onopen = () => (status = { text: 'Live', tone: 'ok' });
    source.onmessage = (event) => {
      try {
        events = JSON.parse(event.data);
      } catch {
        status = { text: 'Bad data from the relay', tone: 'bad' };
      }
    };
    // Fires for connection errors and for the relay's own `error` events.
    source.addEventListener('error', (event) => {
      if ((event as MessageEvent).data) {
        status = { text: 'Traffic unavailable', tone: 'bad' };
      } else if (source.readyState === EventSource.CLOSED) {
        // The browser gave up: the session ended or the relay refused the stream (too many open).
        status = { text: 'Disconnected', tone: 'bad' };
        ended = true;
      } else {
        status = { text: 'Reconnecting', tone: 'warn' };
      }
    });
  }

  async function reconnect() {
    feed?.close();
    // An ended session would only fail again; check it first so the user lands on login.
    await attempt(async () => {
      await api('/api/me');
      openFeed();
    });
  }

  onMount(() => {
    attempt(async () => {
      events = await api<TrafficEvent[]>('/api/traffic/recent', { errors: { 503: 'Traffic is unavailable right now.' } });
      openFeed();
    });
    return () => feed?.close();
  });

  const statusClass = (code: number) => (code >= 500 ? 'bad' : code >= 400 ? 'warn' : 'ok');
</script>

<PageHead eyebrow="requests" title="Recent traffic">The latest requests through your tunnels, updated every two seconds.</PageHead>

<section class="card flush traffic-card">
  <div class="bar traffic-head">
    <h2>Requests</h2>
    <span class="badge {status.tone}">{status.text}</span>
    {#if ended}<button type="button" class="sm" onclick={reconnect}>Reconnect</button>{/if}
  </div>
  {#if sorted?.length}
    <table>
      <thead><tr><th>Time</th><th>Tunnel</th><th>Method</th><th>Status</th><th>In</th><th>Out</th></tr></thead>
      <tbody>
        {#each sorted as e, i (i)}
          <tr>
            <td data-label="Time" class="mono">{new Date(e.timestamp_ms).toLocaleTimeString()}</td>
            <td data-label="Tunnel" class="mono">{e.subdomain}</td>
            <td data-label="Method"><span class="badge plain">{e.method}</span></td>
            <td data-label="Status"><span class="badge plain {statusClass(e.status)}">{e.status}</span></td>
            <td data-label="In" class="num">{formatBytes(e.bytes_in)}</td>
            <td data-label="Out" class="num">{formatBytes(e.bytes_out)}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else if sorted}
    <div class="empty traffic-empty">
      <span class="empty-icon" aria-hidden="true"><Icon name="traffic" size={28} /></span>
      <strong>No requests yet</strong>
      <p>Traffic through your tunnels will appear here when an agent receives requests.</p>
    </div>
  {/if}
</section>

<style>
  .traffic-card { gap: 0; }
  .traffic-head { padding: 16px 18px 8px; }
  .traffic-empty { min-height: 250px; display: grid; place-items: center; align-content: center; gap: 8px; }
  .empty-icon {
    width: 60px; height: 60px; margin-bottom: 4px;
    display: grid; place-items: center; border-radius: 50%;
    color: var(--text-dim); background: var(--active-surface);
  }
  .traffic-empty strong { color: var(--text); font-size: 1rem; }
  .traffic-empty p { max-width: 560px; font-size: 0.86rem; }
  .num { font-variant-numeric: tabular-nums; }
</style>
