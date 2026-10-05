<script lang="ts">
  import { onMount } from 'svelte';
  import { api, ApiError, type Config, type ReservationRequest } from './lib/api';
  import { attempt, currentView, loadMe, notify, session } from './lib/app.svelte';
  import ConfirmDialog from './components/ConfirmDialog.svelte';
  import Toasts from './components/Toasts.svelte';
  import Auth from './pages/Auth.svelte';
  import Shell from './Shell.svelte';

  let ready = $state(false);

  onMount(async () => {
    try {
      session.config = await api<Config>('/api/config');
    } catch (error) {
      notify(`Cannot load the relay settings: ${(error as Error).message}`);
      return;
    }
    try {
      await loadMe();
    } catch (error) {
      if (!(error instanceof ApiError && error.status === 401)) notify((error as Error).message);
    }
    ready = true;
  });

  // Shows pending reservation requests on the admin link from any page.
  $effect(() => {
    const me = session.me;
    if (!me?.is_admin || me.must_change_password) return;
    attempt(async () => {
      session.pendingRequests = (await api<ReservationRequest[]>('/api/admin/reservation-requests')).length;
    });
  });
</script>

{#if ready}
  {#if session.me}
    <Shell view={currentView()} />
  {:else}
    <Auth />
  {/if}
{/if}
<Toasts />
<ConfirmDialog />
