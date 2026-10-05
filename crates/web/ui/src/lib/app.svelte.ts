// Shared app state: session, notices, confirmations, routing and theme.

import { api, ApiError, PASSWORD_CHANGE_REQUIRED, type Config, type Me } from './api';

export const session = $state({ config: null as Config | null, me: null as Me | null, pendingRequests: 0 });

// --- notices -----------------------------------------------------------------

export type Toast = { id: number; message: string; ok: boolean };
export const toasts = $state<Toast[]>([]);
let nextToast = 0;

/** Success notices fade; errors stay until dismissed so they can be read. */
export function notify(message: string, ok = false) {
  const id = nextToast++;
  toasts.push({ id, message, ok });
  if (ok) setTimeout(() => dismiss(id), 5000);
}
export function dismiss(id: number) {
  const index = toasts.findIndex((t) => t.id === id);
  if (index >= 0) toasts.splice(index, 1);
}

// --- confirmations -----------------------------------------------------------

export type Confirmation = { title: string; message: string; action: string; danger: boolean; resolve: (ok: boolean) => void };
export const confirmation = $state({ current: null as Confirmation | null });

export function confirm(title: string, message: string, action: string, danger = true): Promise<boolean> {
  return new Promise((resolve) => {
    confirmation.current = { title, message, action, danger, resolve };
  });
}

// --- session -----------------------------------------------------------------

export async function loadMe() {
  session.me = await api<Me>('/api/me');
}

export async function signOut(message?: string) {
  session.me = null;
  try {
    session.config = await api<Config>('/api/config');
  } catch (error) {
    notify((error as Error).message);
  }
  if (message) notify(message, true);
}

/** Runs an action and reports any failure on screen. A lost session sends the user to login. */
export async function attempt<T>(action: () => Promise<T>): Promise<T | undefined> {
  try {
    return await action();
  } catch (error) {
    if (error instanceof ApiError && error.status === 401 && session.me) {
      await signOut();
      notify('Your session ended. Log in again.');
    } else if (error instanceof ApiError && error.message === PASSWORD_CHANGE_REQUIRED && session.me) {
      // Another session reset the password; reloading shows the forced form.
      await attempt(loadMe);
    } else {
      notify(error instanceof Error ? error.message : String(error));
    }
    return undefined;
  }
}

// --- routing -----------------------------------------------------------------

export const VIEWS = ['overview', 'tunnels', 'tokens', 'names', 'traffic', 'account', 'admin'] as const;
export type View = (typeof VIEWS)[number];

const hash = $state({ value: location.hash.slice(1) });
addEventListener('hashchange', () => { hash.value = location.hash.slice(1); });

export function currentView(): View {
  const me = session.me;
  if (me?.must_change_password) return 'account';
  const view = hash.value as View;
  if (!VIEWS.includes(view) || (view === 'admin' && !me?.is_admin)) return 'overview';
  return view;
}

// --- theme -------------------------------------------------------------------

export const theme = $state({ value: document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light' });

export function toggleTheme() {
  theme.value = theme.value === 'dark' ? 'light' : 'dark';
  document.documentElement.dataset.theme = theme.value;
  try {
    localStorage.setItem('vorp-theme', theme.value);
  } catch {
    // Storage unavailable: the choice still applies, it is just not remembered.
  }
}
