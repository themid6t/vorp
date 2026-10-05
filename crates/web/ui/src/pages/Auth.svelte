<script lang="ts">
  import { post } from '../lib/api';
  import { attempt, dismiss, loadMe, session, theme, toasts, toggleTheme } from '../lib/app.svelte';
  import Icon from '../components/Icon.svelte';

  type Mode = 'bootstrap' | 'login' | 'signup';
  const MODES = {
    bootstrap: { title: 'Create the admin account', submit: 'Create admin', path: '/api/bootstrap',
      help: 'No accounts exist yet. The first account becomes the administrator.',
      errors: { 409: 'An admin account already exists. Log in instead.' } },
    login: { title: 'Welcome back', submit: 'Log in', path: '/api/login', help: 'Log in to manage your tunnels.',
      errors: { 401: 'Wrong email or password.' } },
    signup: { title: 'Create an account', submit: 'Sign up', path: '/api/signup', help: 'Open tunnels to apps on your machine.',
      errors: { 403: 'Signup is closed on this relay. Ask an administrator for an account.', 409: 'That email is already registered.' } },
  } satisfies Record<Mode, unknown>;

  let mode = $state<Mode>(session.config?.needs_bootstrap ? 'bootstrap' : 'login');
  let email = $state('');
  let password = $state('');
  let busy = $state(false);
  const m = $derived(MODES[mode]);
  // Signing up is offered only when the server allows it.
  const canSwitch = $derived(mode !== 'bootstrap' && session.config?.signup_mode === 'open');

  function clearErrors() {
    for (const t of [...toasts]) if (!t.ok) dismiss(t.id);
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    await attempt(async () => {
      try {
        await post(m.path, { email, password }, m.errors);
      } catch (error) {
        // Someone else finished the first-run setup in the meantime.
        if (mode === 'bootstrap' && (error as { status?: number }).status === 409) {
          if (session.config) session.config.needs_bootstrap = false;
          mode = 'login';
        }
        throw error;
      }
      // An account exists now, so a later logout must land on login, not first-run setup.
      if (session.config) session.config.needs_bootstrap = false;
      clearErrors();
      await loadMe();
    });
    busy = false;
  }
</script>

<div class="auth">
  <div class="top">
    <span class="brand"><span class="mark" aria-hidden="true">v</span>vorp</span>
    <button type="button" class="icon" onclick={toggleTheme} aria-label="Switch to {theme.value === 'dark' ? 'light' : 'dark'} theme">
      <Icon name={theme.value === 'dark' ? 'sun' : 'moon'} />
    </button>
  </div>

  <form class="card" onsubmit={submit}>
    <div class="head">
      <p class="eyebrow">self-hosted tunnels</p>
      <h1>{m.title}</h1>
      <p class="dim">{m.help}</p>
    </div>
    <label>Email <input bind:value={email} type="email" autocomplete="username" required></label>
    <label>Password
      <input bind:value={password} type="password" minlength="12" required
        autocomplete={mode === 'login' ? 'current-password' : 'new-password'}>
    </label>
    <button type="submit" class="primary" disabled={busy}>{m.submit}</button>
    {#if canSwitch}
      <p class="switch">
        <button type="button" class="link" onclick={() => { clearErrors(); mode = mode === 'login' ? 'signup' : 'login'; }}>
          {mode === 'login' ? 'Create an account' : 'I already have an account'}
        </button>
      </p>
    {/if}
    <p class="note small"><strong>Tunnel URLs are public.</strong> Anyone with a URL can reach the app behind it. Protect the app itself.</p>
  </form>
</div>

<style>
  .auth { min-height: 100vh; display: grid; grid-template-rows: auto 1fr; padding: 24px 16px; }
  .top { display: flex; justify-content: space-between; align-items: center; max-width: 1100px; width: 100%; margin: 0 auto; }
  .brand { display: flex; align-items: center; gap: 12px; font-size: 1.35rem; font-weight: 700; letter-spacing: -0.03em; }
  .mark { width: 38px; height: 38px; border-radius: 12px; display: grid; place-items: center; background: var(--accent); color: var(--accent-text); box-shadow: var(--raise-sm); }
  form { width: min(420px, 100%); margin: auto; padding: 36px 32px; gap: 18px; }
  .head { display: grid; gap: 8px; margin-bottom: 6px; }
  .eyebrow { font-size: 0.75rem; font-weight: 600; letter-spacing: 0.1em; text-transform: uppercase; color: var(--accent-ink); }
  .switch { text-align: center; }
  .note { color: var(--text-dim); padding: 12px 14px; border-radius: 12px; box-shadow: var(--inset-sm); }
  .note strong { color: var(--warn); }
  button.primary { padding: 13px; margin-top: 4px; }
</style>
