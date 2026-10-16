<script lang="ts">
  import { post } from '../lib/api';
  import { attempt, dismiss, loadMe, session, toasts } from '../lib/app.svelte';
  import Icon from '../components/Icon.svelte';

  type Mode = 'bootstrap' | 'login' | 'signup';
  const MODES = {
    bootstrap: { title: 'Create the admin account', submit: 'Create admin', path: '/api/bootstrap',
      help: 'No accounts exist yet. The first account becomes the administrator.',
      errors: { 409: 'An admin account already exists. Log in instead.' } },
    login: { title: 'Welcome back.', submit: 'Sign in →', path: '/api/login', help: 'Manage your tunnels and expose local services securely.',
      errors: { 401: 'Wrong email or password.' } },
    signup: { title: 'Create an account', submit: 'Sign up', path: '/api/signup', help: 'Open tunnels to apps on your machine.',
      errors: { 403: 'Signup is closed on this relay. Ask an administrator for an account.', 409: 'That email is already registered.' } },
  } satisfies Record<Mode, unknown>;

  let mode = $state<Mode>(session.config?.needs_bootstrap ? 'bootstrap' : 'login');
  let email = $state('');
  let password = $state('');
  let emailError = $state('');
  let passwordError = $state('');
  let busy = $state(false);
  const m = $derived(MODES[mode]);
  // Signing up is offered only when the server allows it.
  const canSwitch = $derived(mode !== 'bootstrap' && session.config?.signup_mode === 'open');

  function clearErrors() {
    for (const t of [...toasts]) if (!t.ok) dismiss(t.id);
    emailError = '';
    passwordError = '';
  }

  function showValidationError(field: 'email' | 'password', event: Event) {
    event.preventDefault();
    const message = (event.currentTarget as HTMLInputElement).validationMessage;
    if (field === 'email') emailError = message;
    else passwordError = message;
  }

  function refreshValidationError(field: 'email' | 'password', event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    if (field === 'email' && emailError) emailError = input.validity.valid ? '' : input.validationMessage;
    if (field === 'password' && passwordError) passwordError = input.validity.valid ? '' : input.validationMessage;
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
  <section class="panel">
    <header class="top">
      <span class="brand"><span class="mark" aria-hidden="true">v</span>vorp</span>
    </header>

    <main class="content">
      <p class="eyebrow">self-hosted tunnels</p>
      <h1>{m.title}</h1>
      <p class="supporting">{m.help}</p>
      <form onsubmit={submit} aria-busy={busy}>
        <label>{mode === 'login' ? 'Email address' : 'Email'}
          <input bind:value={email} type="email" autocomplete="username" placeholder={mode === 'login' ? 'you@example.com' : undefined} required
            aria-invalid={emailError ? 'true' : undefined} aria-describedby={emailError ? 'email-error' : undefined}
            oninvalid={(event) => showValidationError('email', event)} oninput={(event) => refreshValidationError('email', event)}>
          {#if emailError}<span class="field-error" id="email-error">{emailError}</span>{/if}
        </label>
        <label>Password
          <input bind:value={password} type="password" minlength="12" required
            placeholder={mode === 'login' ? 'Enter your password' : undefined}
            autocomplete={mode === 'login' ? 'current-password' : 'new-password'}
            aria-invalid={passwordError ? 'true' : undefined} aria-describedby={passwordError ? 'password-error' : undefined}
            oninvalid={(event) => showValidationError('password', event)} oninput={(event) => refreshValidationError('password', event)}>
          {#if passwordError}<span class="field-error" id="password-error">{passwordError}</span>{/if}
        </label>
        <button type="submit" class="primary submit" disabled={busy}>
          {busy ? 'Please wait…' : m.submit}
        </button>
        {#if canSwitch}
          <p class="switch">
            <button type="button" class="link" onclick={() => { clearErrors(); mode = mode === 'login' ? 'signup' : 'login'; }}>
              {mode === 'login' ? 'Create an account' : 'I already have an account'}
            </button>
          </p>
        {/if}
      </form>
      <p class="note">
        <span class="note-icon"><Icon name="shield" size={15} /></span>
        {#if mode === 'login'}
          <span><strong>Tunnel URLs are public.</strong> Protect the app behind each tunnel.</span>
        {:else}
          <span><strong>Tunnel URLs are public.</strong> Anyone with a URL can reach the app behind it. Protect the app itself.</span>
        {/if}
      </p>
    </main>

  </section>

  <div class="texture" aria-hidden="true">
    <img src="/auth-tunnel.jpg" alt="">
  </div>
</div>

<style>
  .auth {
    --auth-bg: var(--bg);
    --auth-text: var(--text);
    --auth-muted: var(--text-dim);
    --card-text: var(--color-foreground);
    --auth-input: var(--color-input);
    --auth-border: var(--color-border);
    --auth-focus: var(--color-foreground);
    min-height: 100vh;
    min-height: 100svh;
    display: grid;
    grid-template-columns: minmax(520px, 55fr) minmax(430px, 45fr);
    overflow: hidden;
    background: var(--auth-bg);
    color: var(--auth-text);
  }
  .panel {
    min-width: 0; min-height: 100vh; min-height: 100svh; overflow-y: auto;
    display: grid; grid-template-rows: 40px minmax(0, 1fr);
    padding: clamp(24px, 3.5vh, 38px) clamp(32px, 4.5vw, 76px);
  }
  .top { display: flex; align-items: center; width: 100%; min-width: 0; }
  .brand { display: flex; align-items: center; gap: 10px; font-family: var(--font-sans); font-size: 1.35rem; font-weight: 650; letter-spacing: -0.035em; }
  .mark { width: 30px; height: 30px; display: grid; place-items: center; border-radius: 7px; background: var(--color-primary); color: var(--color-primary-foreground); font-family: var(--font); font-size: 0.95rem; }
  .content { width: min(100%, 430px); margin: auto; padding: 34px 0 24px; text-align: center; }
  .eyebrow { margin-bottom: 12px; color: var(--auth-muted); font-size: 0.68rem; font-weight: 650; letter-spacing: 0.17em; text-transform: uppercase; }
  h1 { margin: 0; color: var(--auth-text); font-family: var(--font-sans); font-size: clamp(2.65rem, 3.6vw, 3.55rem); font-weight: 520; letter-spacing: -0.05em; line-height: 1.02; text-wrap: balance; }
  .supporting { margin: 15px auto 28px; color: var(--auth-text); font-family: var(--font-sans); font-size: 1rem; line-height: 1.45; }
  form { display: grid; gap: 18px; color: var(--auth-text); text-align: left; }
  .auth label { gap: 7px; color: var(--auth-text); font-size: 0.76rem; font-weight: 580; }
  .auth input:not([type='checkbox'], [type='radio']) {
    min-height: 46px; padding: 11px 14px; border: 1px solid var(--auth-border); border-radius: 10px;
    outline: 0; box-shadow: none; background: var(--auth-input); color: var(--auth-text); transition: border-color 140ms ease, background-color 140ms ease;
  }
  .auth input:not([type='checkbox'], [type='radio']) { color: var(--card-text); font-size: 0.85rem; }
  .auth input:hover:not(:disabled) { border-color: var(--auth-muted); }
  .auth input:focus-visible { border-color: var(--auth-focus); outline: 2px solid var(--auth-focus); outline-offset: 2px; }
  .auth input[aria-invalid='true'] { border-color: var(--danger); }
  .auth input[aria-invalid='true']:focus-visible { outline-color: var(--danger); }
  .field-error { color: var(--danger); font-size: 0.72rem; font-weight: 500; line-height: 1.35; }
  .auth button.submit {
    width: 100%; min-height: 46px; margin-top: 2px; border-radius: 10px; border: 1px solid var(--color-primary);
    background: var(--color-primary); color: var(--color-primary-foreground); box-shadow: none; font-size: 0.88rem; font-weight: 580;
  }
  .auth button.submit:hover:not(:disabled) { color: var(--color-primary-foreground); filter: brightness(1.12); }
  .auth button.submit:active:not(:disabled) { box-shadow: none; transform: translateY(1px); }
  .switch { text-align: center; }
  .auth button.link { color: var(--auth-text); font-size: 0.78rem; }
  .note { width: 100%; margin: 20px 0 0; display: flex; align-items: flex-start; gap: 8px; color: var(--auth-muted); font-size: 0.72rem; line-height: 1.5; text-align: left; }
  .note-icon { width: 28px; height: 28px; display: inline-flex; align-items: center; justify-content: center; flex: none; color: var(--card-text); background: var(--auth-input); border: 1px solid var(--auth-border); border-radius: 7px; }
  .note strong { color: var(--auth-text); }
  .texture { min-width: 0; height: calc(100vh - 88px); height: calc(100svh - 88px); margin: 44px 44px 44px 0; overflow: hidden; border-radius: 18px; }
  .texture img { width: 100%; height: 100%; display: block; object-fit: cover; object-position: center; }

  :global(:root[data-theme='dark']) .auth { --auth-focus: var(--color-foreground); }

  @media (max-width: 1000px) {
    .auth { grid-template-columns: minmax(0, 1fr); overflow: auto; }
    .panel { min-height: 100vh; min-height: 100svh; padding: 22px clamp(20px, 8vw, 64px); }
    .texture { display: none; }
    .content { padding: 48px 0 28px; }
    h1 { font-size: clamp(2.7rem, 12vw, 3.8rem); }
  }

  @media (max-height: 760px) and (min-width: 1001px) {
    .panel { padding-block: 18px; }
    .content { padding-block: 24px 18px; }
    .supporting { margin-block: 12px 20px; }
    form { gap: 14px; }
    .note { margin-top: 20px; }
  }

  @media (max-width: 480px) {
    .panel { padding: 18px 20px; }
    .content { padding: 38px 0 24px; }
    h1 { font-size: clamp(2.45rem, 11vw, 3.2rem); }
  }
</style>
