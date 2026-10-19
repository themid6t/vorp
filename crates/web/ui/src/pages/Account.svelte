<script lang="ts">
  import { post } from '../lib/api';
  import { notify, session, signOut } from '../lib/app.svelte';
  import Icon from '../components/Icon.svelte';
  import PageHead from '../components/PageHead.svelte';

  const forced = $derived(session.me!.must_change_password);
  let oldPassword = $state('');
  let newPassword = $state('');
  let repeat = $state('');
  let busy = $state(false);
  let showOld = $state(false);
  let showNew = $state(false);
  let showRepeat = $state(false);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (newPassword !== repeat) {
      notify('The new passwords do not match.');
      return;
    }
    busy = true;
    try {
      await post('/api/password', { old_password: oldPassword, new_password: newPassword },
        { 401: 'The current password is wrong.' });
      await signOut('Password changed. Log in with your new password.');
    } catch (error) {
      // A 401 here means a wrong current password, not a lost session.
      notify((error as Error).message);
    }
    busy = false;
  }
</script>

<PageHead eyebrow="account" title="Password">Changing it logs you out everywhere, including this browser.</PageHead>

{#if forced}
  <section class="card callout">
    <h2>Choose your own password to continue</h2>
    <p class="dim">An administrator or a reset set your current password. Pick a new one before using the dashboard.</p>
  </section>
{/if}

<form class="card narrow" onsubmit={submit}>
  <div class="form-title"><span class="title-icon" aria-hidden="true"><Icon name="lock" size={18} /></span><h2>Change password</h2></div>
  <label>Current password
    <span class="password-field">
      <input bind:value={oldPassword} type={showOld ? 'text' : 'password'} autocomplete="current-password" required>
      <button type="button" class="visibility" onclick={() => (showOld = !showOld)} aria-label={showOld ? 'Hide current password' : 'Show current password'} title={showOld ? 'Hide password' : 'Show password'}>
        <Icon name={showOld ? 'eyeOff' : 'eye'} size={17} />
      </button>
    </span>
  </label>
  <label>New password
    <span class="password-field">
      <input bind:value={newPassword} type={showNew ? 'text' : 'password'} autocomplete="new-password" minlength="12" required>
      <button type="button" class="visibility" onclick={() => (showNew = !showNew)} aria-label={showNew ? 'Hide new password' : 'Show new password'} title={showNew ? 'Hide password' : 'Show password'}>
        <Icon name={showNew ? 'eyeOff' : 'eye'} size={17} />
      </button>
    </span>
  </label>
  <label>Repeat new password
    <span class="password-field">
      <input bind:value={repeat} type={showRepeat ? 'text' : 'password'} autocomplete="new-password" minlength="12" required>
      <button type="button" class="visibility" onclick={() => (showRepeat = !showRepeat)} aria-label={showRepeat ? 'Hide repeated password' : 'Show repeated password'} title={showRepeat ? 'Hide password' : 'Show password'}>
        <Icon name={showRepeat ? 'eyeOff' : 'eye'} size={17} />
      </button>
    </span>
  </label>
  <p class="dim small">At least 12 characters.</p>
  <div class="form-actions"><button type="submit" class="primary" disabled={busy}>Change password</button></div>
</form>

<style>
  .narrow { max-width: 520px; gap: 14px; padding: 18px 20px; }
  .form-title { display: flex; align-items: center; gap: 10px; margin-bottom: 2px; }
  .title-icon { width: 32px; height: 32px; display: grid; place-items: center; border: 1px solid var(--color-border); border-radius: 7px; color: var(--text-dim); background: var(--active-surface); }
  .password-field { position: relative; display: block; }
  .password-field input { padding-right: 46px; }
  .visibility {
    position: absolute; top: 50%; right: 7px; transform: translateY(-50%);
    width: 32px; height: 32px; min-height: 32px; padding: 0;
    color: var(--text-dim); background: transparent; border-color: transparent;
  }
  .visibility:hover:not(:disabled) { color: var(--text); background: var(--hover-surface); filter: none; }
  .form-actions { display: flex; justify-content: flex-end; margin-top: 4px; padding-top: 14px; border-top: 1px solid var(--color-border); }
  .callout { max-width: 480px; gap: 8px; }
  .callout h2 { color: var(--warn); }
</style>
