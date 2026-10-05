<script lang="ts">
  import { post } from '../lib/api';
  import { notify, session, signOut } from '../lib/app.svelte';
  import PageHead from '../components/PageHead.svelte';

  const forced = $derived(session.me!.must_change_password);
  let oldPassword = $state('');
  let newPassword = $state('');
  let repeat = $state('');
  let busy = $state(false);

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
  <label>Current password <input bind:value={oldPassword} type="password" autocomplete="current-password" required></label>
  <label>New password <input bind:value={newPassword} type="password" autocomplete="new-password" minlength="12" required></label>
  <label>Repeat new password <input bind:value={repeat} type="password" autocomplete="new-password" minlength="12" required></label>
  <p class="dim small">At least 12 characters.</p>
  <div><button type="submit" class="primary" disabled={busy}>Change password</button></div>
</form>

<style>
  .narrow { max-width: 480px; }
  .callout { max-width: 480px; gap: 8px; }
  .callout h2 { color: var(--warn); }
</style>
