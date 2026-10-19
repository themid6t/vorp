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
  const accountName = $derived(displayName(me.email));
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
  let accountOpen = $state(false);
  let nav = $state<HTMLElement>();
  let accountArea = $state<HTMLElement>();
  let accountTrigger = $state<HTMLButtonElement>();

  function displayName(email: string) {
    const local = email.split('@', 1)[0] ?? email;
    const words = local.replace(/[._-]+/g, ' ').replace(/\d+$/g, '').trim().split(/\s+/).filter(Boolean);
    return (words.length ? words : [local]).map((word) => word.charAt(0).toUpperCase() + word.slice(1)).join(' ');
  }

  // On narrow screens the nav scrolls sideways; keep the current page's link visible.
  $effect(() => {
    nav?.querySelector(`a[href="#${view}"]`)?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  });

  async function logout() {
    leaving = true;
    const signedOut = await attempt(async () => {
      await post('/api/logout');
      await signOut('Logged out.');
      return true;
    });
    if (signedOut) accountOpen = false;
    leaving = false;
  }

  function closeAccountMenuOnOutsideClick(event: PointerEvent) {
    if (accountOpen && !accountArea?.contains(event.target as Node)) accountOpen = false;
  }

  function closeAccountMenuOnEscape(event: KeyboardEvent) {
    if (event.key === 'Escape' && accountOpen) {
      accountOpen = false;
      accountTrigger?.focus();
    }
  }
</script>

<svelte:window onpointerdown={closeAccountMenuOnOutsideClick} onkeydown={closeAccountMenuOnEscape} />

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
    <div class="foot" bind:this={accountArea}>
      <div class="account-row">
        <button
          type="button"
          class="account-trigger"
          aria-expanded={accountOpen}
          aria-controls="account-menu"
          aria-haspopup="menu"
          bind:this={accountTrigger}
          onclick={() => (accountOpen = !accountOpen)}
        >
          <span class="who" title={me.email}><strong>{accountName}</strong><span class="dim">{me.email}</span></span>
          <span class:open={accountOpen} class="chevron"><Icon name="chevron" size={15} /></span>
        </button>
      </div>
      {#if accountOpen}
        <div class="account-menu" id="account-menu" role="menu">
          <div class="menu-head">
            <span class="menu-identity"><strong>{accountName}</strong><span>{me.email}</span></span>
          </div>
          <div class="menu-divider"></div>
          <button type="button" class="menu-item" role="menuitem" onclick={logout} disabled={leaving}>
            <Icon name="logout" size={16} />{leaving ? 'Signing out…' : 'Sign out'}
          </button>
        </div>
      {/if}
    </div>
  </aside>

  <header class="app-header">
    <button
      type="button"
      class="icon theme-toggle"
      onclick={toggleTheme}
      aria-label="Switch to {theme.value === 'dark' ? 'light' : 'dark'} mode"
      title="Switch to {theme.value === 'dark' ? 'light' : 'dark'} mode"
    >
      <Icon name={theme.value === 'dark' ? 'moon' : 'sun'} size={18} />
    </button>
  </header>

  <main>
    {#key view}
      <Page />
    {/key}
  </main>
</div>

<style>
  .layout { display: grid; grid-template-columns: 200px minmax(0, 1fr); grid-template-rows: 44px minmax(0, 1fr); min-height: 100vh; }
  aside {
    position: sticky; top: 0; grid-row: 1 / -1; height: 100vh;
    display: flex; flex-direction: column; gap: 20px;
    padding: 16px 12px;
    background: var(--sidebar);
    border-right: 1px solid var(--layout-border);
  }
  .brand { display: flex; align-items: center; gap: 9px; font-size: 1.1rem; font-weight: 600; letter-spacing: -0.02em; color: var(--text); padding: 0 6px; }
  .brand:hover { text-decoration: none; }
  .mark { width: 30px; height: 30px; border-radius: 6px; display: grid; place-items: center; background: var(--accent); color: var(--accent-text); font-size: 0.9rem; }
  nav { display: grid; gap: 4px; }
  nav a {
    display: flex; align-items: center; gap: 10px;
    min-height: 36px; padding: 6px 10px;
    border: 1px solid transparent;
    border-radius: 6px;
    color: var(--text-dim);
    font-size: 0.875rem;
    font-weight: 500;
    transition: background-color 0.15s, color 0.15s;
  }
  nav a:hover { text-decoration: none; color: var(--text); background: var(--hover-surface); }
  nav a[aria-current='page'] { color: var(--text); background: var(--active-surface); border-color: var(--color-border); }
  .count { margin-left: auto; min-width: 22px; height: 22px; padding: 0 6px; border-radius: 999px; background: var(--accent); color: var(--accent-text); font-size: 0.75rem; font-weight: 700; display: grid; place-items: center; }
  .foot { position: relative; margin-top: auto; padding-top: 14px; border-top: 1px solid var(--color-border); }
  .account-row { min-width: 0; }
  .account-trigger {
    width: 100%; min-width: 0; min-height: 44px; padding: 5px 7px;
    justify-content: flex-start; color: var(--text); background: transparent;
    border-color: transparent; text-align: left;
  }
  .account-trigger:hover:not(:disabled) { color: var(--text); background: var(--hover-surface); filter: none; }
  .account-trigger:active:not(:disabled) { background: var(--active-surface); filter: none; }
  .who { min-width: 0; flex: 1; display: grid; overflow: hidden; white-space: nowrap; font-size: 0.76rem; line-height: 1.3; }
  .who strong, .who > span { overflow: hidden; text-overflow: ellipsis; }
  .who strong { font-weight: 600; }
  .who > span { font-size: 0.65rem; font-weight: 400; }
  .chevron { display: grid; place-items: center; flex: none; color: var(--text-dim); transition: transform 0.15s ease; }
  .chevron.open { transform: rotate(180deg); }
  .account-menu {
    position: absolute; z-index: 10; right: 0; bottom: calc(100% + 8px); left: 0;
    padding: 8px; background: var(--popover); border: 1px solid var(--color-border);
    border-radius: 7px; box-shadow: var(--panel-shadow);
  }
  .menu-head { display: flex; gap: 8px; align-items: center; min-width: 0; padding: 2px 2px 8px; }
  .menu-identity { min-width: 0; flex: 1; display: grid; line-height: 1.35; }
  .menu-identity strong { font-size: 0.78rem; font-weight: 600; }
  .menu-identity span { color: var(--text-dim); font-size: 0.67rem; overflow-wrap: anywhere; }
  .menu-divider { height: 1px; margin: 2px 0 4px; background: var(--color-border); }
  .menu-item {
    width: 100%; min-height: 34px; justify-content: flex-start; padding: 6px 9px;
    color: var(--text); background: transparent; border-color: transparent; font-size: 0.8125rem;
  }
  .menu-item :global(svg) { color: var(--text-dim); }
  .menu-item:hover:not(:disabled) { color: var(--text); background: var(--hover-surface); filter: none; }
  .app-header {
    display: flex; align-items: center; justify-content: flex-end;
    min-width: 0; padding: 4px clamp(16px, 3vw, 36px);
    background: var(--bg);
  }
  .theme-toggle { color: var(--text-dim); background: var(--color-surface); border-color: var(--color-border); box-shadow: var(--card-shadow); }
  .theme-toggle:hover:not(:disabled) { color: var(--text); background: var(--hover-surface); filter: none; }
  main { margin: 0 auto; padding: 8px clamp(16px, 3vw, 36px) 32px; display: grid; gap: 14px; align-content: start; max-width: 1160px; width: 100%; }

  @media (max-width: 900px) {
    .layout { grid-template-columns: minmax(0, 1fr); grid-template-rows: auto 44px auto; }
    aside { position: static; grid-row: auto; height: auto; padding: 12px 16px; gap: 12px; flex-direction: row; flex-wrap: wrap; align-items: center; border-right: 0; border-bottom: 1px solid var(--layout-border); }
    nav { order: 3; width: 100%; min-width: 0; display: flex; overflow-x: auto; padding: 6px; gap: 6px; scrollbar-width: none; }
    nav a { flex: none; padding: 9px 12px; }
    .foot { margin: 0 0 0 auto; padding: 0; border: 0; }
    .account-menu { right: 0; left: auto; width: min(280px, calc(100vw - 32px)); }
    .account-trigger { max-width: 190px; }
    .app-header { padding-inline: 16px; }
    main { padding: 8px 16px 28px; gap: 14px; }
  }
</style>
