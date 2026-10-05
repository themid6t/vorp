<script lang="ts">
  import { post } from './lib/api';
  import { attempt, session, signOut, theme, toggleTheme, type View } from './lib/app.svelte';
  import Icon from './components/Icon.svelte';
  import Overview from './pages/Overview.svelte';
  import Tunnels from './pages/Tunnels.svelte';
  import Tokens from './pages/Tokens.svelte';
  import Names from './pages/Names.svelte';
  import Traffic from './pages/Traffic.svelte';
  import Account from './pages/Account.svelte';
  import Admin from './pages/Admin.svelte';

  let { view }: { view: View } = $props();
  const me = $derived(session.me!);
  const PAGES = { overview: Overview, tunnels: Tunnels, tokens: Tokens, names: Names, traffic: Traffic, account: Account, admin: Admin };
  const LINKS: { view: View; label: string }[] = [
    { view: 'overview', label: 'Overview' },
    { view: 'tunnels', label: 'Tunnels' },
    { view: 'tokens', label: 'Tokens' },
    { view: 'names', label: 'Subdomains' },
    { view: 'traffic', label: 'Traffic' },
    { view: 'account', label: 'Account' },
  ];
  const Page = $derived(PAGES[view]);
  let leaving = $state(false);
  let nav = $state<HTMLElement>();

  // On narrow screens the nav scrolls sideways; keep the current page's link visible.
  $effect(() => {
    nav?.querySelector(`a[href="#${view}"]`)?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  });

  async function logout() {
    leaving = true;
    await attempt(async () => {
      await post('/api/logout');
      await signOut('Logged out.');
    });
    leaving = false;
  }
</script>

<div class="layout">
  <aside>
    <a class="brand" href="#overview"><span class="mark" aria-hidden="true">v</span>vorp</a>
    <!-- Until the user picks their own password the API refuses everything else. -->
    {#if !me.must_change_password}
      <nav aria-label="Main" bind:this={nav}>
        {#each LINKS as link (link.view)}
          <a href="#{link.view}" aria-current={view === link.view ? 'page' : undefined}><Icon name={link.view} />{link.label}</a>
        {/each}
        {#if me.is_admin}
          <a href="#admin" aria-current={view === 'admin' ? 'page' : undefined}>
            <Icon name="admin" />Admin
            {#if session.pendingRequests}<span class="count" aria-label="{session.pendingRequests} pending requests">{session.pendingRequests}</span>{/if}
          </a>
        {/if}
      </nav>
    {/if}
    <div class="foot">
      <p class="who" title={me.email}>{me.email}<span class="dim small">{me.is_admin ? 'Administrator' : 'User'}</span></p>
      <div class="row">
        <button type="button" class="icon" onclick={toggleTheme} aria-label="Switch to {theme.value === 'dark' ? 'light' : 'dark'} theme">
          <Icon name={theme.value === 'dark' ? 'sun' : 'moon'} />
        </button>
        <button type="button" class="grow" onclick={logout} disabled={leaving}><Icon name="logout" size={16} />Sign out</button>
      </div>
    </div>
  </aside>

  <main>
    <p class="note" role="note"><strong>Tunnel URLs are public.</strong> Anyone with a URL can reach the app behind it. A URL is not access control, so protect the app itself.</p>
    {#key view}
      <Page />
    {/key}
  </main>
</div>

<style>
  .layout { display: grid; grid-template-columns: 260px minmax(0, 1fr); min-height: 100vh; }
  aside {
    position: sticky; top: 0; height: 100vh;
    display: flex; flex-direction: column; gap: 28px;
    padding: 28px 20px;
  }
  .brand { display: flex; align-items: center; gap: 12px; font-size: 1.35rem; font-weight: 700; letter-spacing: -0.03em; color: var(--text); padding: 0 8px; }
  .brand:hover { text-decoration: none; }
  .mark { width: 38px; height: 38px; border-radius: 12px; display: grid; place-items: center; background: var(--accent); color: var(--accent-text); box-shadow: var(--raise-sm); font-size: 1.15rem; }
  nav { display: grid; gap: 8px; }
  nav a {
    display: flex; align-items: center; gap: 12px;
    padding: 11px 14px;
    border-radius: 14px;
    color: var(--text-dim);
    font-weight: 500;
    transition: box-shadow 0.15s, color 0.15s;
  }
  nav a:hover { text-decoration: none; color: var(--text); box-shadow: var(--raise-sm); }
  nav a[aria-current='page'] { color: var(--accent-ink); box-shadow: var(--inset); }
  .count { margin-left: auto; min-width: 22px; height: 22px; padding: 0 6px; border-radius: 999px; background: var(--accent); color: var(--accent-text); font-size: 0.75rem; font-weight: 700; display: grid; place-items: center; }
  .foot { margin-top: auto; display: grid; gap: 14px; }
  .who { display: grid; padding: 12px 14px; border-radius: 14px; box-shadow: var(--inset-sm); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 550; font-size: 0.9rem; }
  main { padding: 36px clamp(16px, 4vw, 48px) 64px; display: grid; gap: 28px; align-content: start; max-width: 1180px; width: 100%; }
  .note { font-size: 0.85rem; color: var(--text-dim); padding: 12px 16px; border-radius: 14px; box-shadow: var(--inset-sm); }
  .note strong { color: var(--warn); }

  @media (max-width: 900px) {
    .layout { grid-template-columns: minmax(0, 1fr); }
    aside { position: static; height: auto; padding: 16px; gap: 16px; flex-direction: row; flex-wrap: wrap; align-items: center; }
    nav { order: 3; width: 100%; min-width: 0; display: flex; overflow-x: auto; padding: 6px; gap: 6px; scrollbar-width: none; }
    nav a { flex: none; padding: 9px 12px; }
    .foot { margin: 0 0 0 auto; display: flex; align-items: center; }
    .who { display: none; }
    main { padding-top: 8px; }
  }
</style>
