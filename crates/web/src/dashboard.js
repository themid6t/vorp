'use strict';
// vorp dashboard. Plain DOM, no dependencies; everything is served from the relay binary.
// Content is always set with textContent, never innerHTML, so API data cannot inject markup.

const MIB = 1024 * 1024;
const THEME_KEY = 'vorp-theme';

// Runs before the body paints so a stored light theme never flashes dark.
// Storage can be unavailable (private windows, blocked site data); the
// system preference is the fallback.
function applyTheme(theme) {
  document.documentElement.dataset.theme = theme;
}
function storedTheme() {
  try {
    const value = localStorage.getItem(THEME_KEY);
    if (value === 'light' || value === 'dark') return value;
  } catch { /* Storage unavailable: use the system preference. */ }
  return matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
}
function toggleTheme() {
  const next = document.documentElement.dataset.theme === 'light' ? 'dark' : 'light';
  applyTheme(next);
  try { localStorage.setItem(THEME_KEY, next); } catch { /* Not remembered; still applied. */ }
}
applyTheme(storedTheme());
const state = { config: null, me: null, reservations: [], view: null, timer: null, feed: null };

const $ = (id) => document.getElementById(id);

// --- API ---------------------------------------------------------------------

class ApiError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

const FALLBACK = {
  0: 'Cannot reach the relay. Check your connection and try again.',
  400: 'The relay rejected the request.',
  401: 'You are not logged in.',
  403: 'You are not allowed to do that.',
  404: 'Not found.',
  409: 'That conflicts with an existing record.',
  413: 'The request is too large.',
  429: 'Too many requests. Wait a moment and try again.',
  500: 'The relay hit an internal error.',
  503: 'The relay is temporarily unavailable. Try again.',
};

// `errors` maps a status code to a message that explains it for this call.
async function api(path, { method = 'GET', body, errors = {} } = {}) {
  const init = { method, credentials: 'same-origin', headers: {} };
  if (method !== 'GET') init.headers['x-vorp-csrf'] = '1';
  if (body !== undefined) {
    init.headers['content-type'] = 'application/json';
    init.body = JSON.stringify(body);
  }
  let response;
  try {
    response = await fetch(path, init);
  } catch {
    throw new ApiError(0, FALLBACK[0]);
  }
  const isJson = (response.headers.get('content-type') || '').includes('application/json');
  const data = isJson ? await response.json().catch(() => null) : null;
  if (response.ok) return data;
  const text = isJson ? '' : (await response.text().catch(() => '')).trim().slice(0, 200);
  const status = response.status;
  // Specific explanation first, then the server's message, then a generic one.
  const message = errors[status] || explain(data && data.error) || text || FALLBACK[status] || `Request failed (HTTP ${status}).`;
  const retry = response.headers.get('retry-after');
  throw new ApiError(status, retry && status === 429 ? `${message} (retry in ${retry}s)` : message);
}

// The relay's error strings are terse codes; spell out the ones that need it.
function explain(error) {
  if (!error) return '';
  const known = {
    unauthorized: FALLBACK[401],
    forbidden: FALLBACK[403],
    conflict: FALLBACK[409],
    'rate limit exceeded': FALLBACK[429],
    'relay disconnect unavailable': FALLBACK[503],
    'internal error': FALLBACK[500],
  };
  return known[error] || error.charAt(0).toUpperCase() + error.slice(1) + '.';
}

// Runs an action and reports any failure on screen. A lost session sends the user to login.
async function attempt(action, button) {
  if (button) button.disabled = true;
  try {
    return await action();
  } catch (error) {
    if (error instanceof ApiError && error.status === 401 && state.me) {
      signedOut('Your session ended. Log in again.');
    } else if (error instanceof ApiError && error.message === 'Password change required.' && state.me) {
      // Another session reset the password; reload so the forced form shows.
      attempt(enterApp);
    } else {
      notify(error.message || String(error));
    }
  } finally {
    if (button) button.disabled = false;
  }
}

function notify(message, ok = false) {
  $('notice-text').textContent = message;
  $('notice').className = ok ? 'ok' : '';
  $('notice').hidden = false;
  clearTimeout(notify.timer);
  if (ok) notify.timer = setTimeout(() => { $('notice').hidden = true; }, 5000);
}
function clearNotice() { $('notice').hidden = true; }

// --- DOM helpers -------------------------------------------------------------

function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (key.startsWith('on')) el.addEventListener(key.slice(2), value);
    else if (value !== false && value != null) el.setAttribute(key, value === true ? '' : value);
  }
  for (const child of children.flat()) {
    if (child != null) el.append(child instanceof Node ? child : String(child));
  }
  return el;
}

// columns: [{label, cell(row) -> node|string, className?}]
function table(rows, columns, emptyText) {
  if (!rows.length) return h('p', { class: 'empty' }, emptyText);
  return h('table', {},
    h('thead', {}, h('tr', {}, columns.map((c) => h('th', {}, c.label)))),
    h('tbody', {}, rows.map((row) => h('tr', {},
      columns.map((c) => h('td', { 'data-label': c.label, class: c.className }, c.cell(row)))))));
}

function render(id, node) { $(id).replaceChildren(node); }

function formData(form) { return Object.fromEntries(new FormData(form)); }

function tunnelUrl(name) { return `https://${name}.${state.config.base_domain}`; }
function tunnelLink(name) { return h('a', { href: tunnelUrl(name), target: '_blank', rel: 'noopener noreferrer' }, tunnelUrl(name)); }

function formatRate(bytesPerSec) {
  if (bytesPerSec >= MIB) return `${+(bytesPerSec / MIB).toFixed(2)} MiB/s`;
  if (bytesPerSec >= 1024) return `${+(bytesPerSec / 1024).toFixed(1)} KiB/s`;
  return `${bytesPerSec} B/s`;
}
function formatBytes(bytes) {
  if (bytes >= MIB) return `${+(bytes / MIB).toFixed(1)} MiB`;
  if (bytes >= 1024) return `${+(bytes / 1024).toFixed(1)} KiB`;
  return `${bytes} B`;
}
function mibToBytes(value) { return Math.round(Number(value) * MIB); }
function bytesToMib(bytes) { return +(bytes / MIB).toFixed(3); }

async function copy(text, button) {
  try {
    await navigator.clipboard.writeText(text);
    const label = button.textContent;
    button.textContent = 'Copied';
    setTimeout(() => { button.textContent = label; }, 1500);
  } catch {
    notify('Copying failed. Select the text and copy it by hand.');
  }
}

// --- Agent command -----------------------------------------------------------

// The agent dials port 443 of --relay-host, which must match the relay certificate.
// A relay on another port also needs --relay-addr, which takes an IP address.
function agentCommand(rawToken, names = []) {
  const parts = [`VORP_TOKEN=${rawToken || '<your-token>'}`, 'vorp', '--relay-host', state.config.base_domain];
  const port = location.port;
  if (port && port !== '443') parts.push('--relay-addr', `${relayIp()}:${port}`);
  for (const name of names) parts.push('--subdomain', name);
  parts.push('--upstream', 'http://127.0.0.1:3000');
  return parts.join(' ');
}
function relayIp() {
  const host = location.hostname;
  if (host === 'localhost') return '127.0.0.1';
  if (/^[\d.]+$/.test(host) || host.startsWith('[')) return host;
  return '<relay-ip>';
}
function commandNote() {
  const port = location.port;
  return port && port !== '443'
    ? 'This relay is not on port 443, so the agent needs --relay-addr. With a self-signed certificate, also pass --ca-cert <file>.'
    : '';
}
function showCommand(preId, noteId, rawToken, names) {
  $(preId).textContent = agentCommand(rawToken, names);
  const note = commandNote();
  $(noteId).textContent = note;
  $(noteId).hidden = !note;
}

// --- Auth --------------------------------------------------------------------

const AUTH_MODES = {
  bootstrap: { title: 'Create the admin account', submit: 'Create admin', path: '/api/bootstrap',
    help: 'No accounts exist yet. The first account becomes the administrator.' },
  login: { title: 'Log in', submit: 'Log in', path: '/api/login', help: '' },
  signup: { title: 'Create an account', submit: 'Sign up', path: '/api/signup', help: '' },
};
let authMode = 'login';

function setAuthMode(mode) {
  authMode = mode;
  const m = AUTH_MODES[mode];
  const signupMode = state.config ? state.config.signup_mode : 'closed';
  $('auth-title').textContent = m.title;
  $('auth-submit').textContent = m.submit;
  $('auth-help').textContent = m.help;
  $('auth-help').hidden = !m.help;
  $('auth-form').elements.password.autocomplete = mode === 'login' ? 'current-password' : 'new-password';
  // Signing up is offered only when the server allows it.
  const canSwitch = mode !== 'bootstrap' && signupMode !== 'closed';
  $('auth-switch').hidden = !canSwitch;
  $('auth-toggle').textContent = mode === 'login' ? 'Create an account' : 'I already have an account';
}

function showAuth(message) {
  stopViewWork();
  state.me = null;
  $('app').hidden = true;
  $('whoami').hidden = true;
  $('logout').hidden = true;
  $('auth').hidden = false;
  setAuthMode(state.config && state.config.needs_bootstrap ? 'bootstrap' : 'login');
  if (message) notify(message, true);
}

async function signedOut(message) {
  await attempt(async () => { state.config = await api('/api/config'); });
  showAuth(message);
}

async function submitAuth(event) {
  event.preventDefault();
  const form = event.target;
  const { email, password } = formData(form);
  const body = { email, password };
  const errors = {
    login: { 401: 'Wrong email or password.' },
    signup: { 403: 'Signup is closed on this relay. Ask an administrator for an account.',
      409: 'That email is already registered.' },
    bootstrap: { 409: 'An admin account already exists. Log in instead.' },
  }[authMode];
  await attempt(async () => {
    try {
      await api(AUTH_MODES[authMode].path, { method: 'POST', body, errors });
    } catch (error) {
      // Someone else finished the first-run setup in the meantime.
      if (authMode === 'bootstrap' && error.status === 409) {
        state.config.needs_bootstrap = false;
        setAuthMode('login');
      }
      throw error;
    }
    // An account exists now, so a later logout must land on login, not first-run setup.
    state.config.needs_bootstrap = false;
    form.reset();
    clearNotice();
    await enterApp();
  }, $('auth-submit'));
}

async function logout() {
  await attempt(async () => {
    await api('/api/logout', { method: 'POST' });
    showAuth('Logged out.');
  }, $('logout'));
}

// --- App shell ---------------------------------------------------------------

async function enterApp() {
  state.me = await api('/api/me');
  $('auth').hidden = true;
  $('app').hidden = false;
  $('whoami').textContent = state.me.email;
  $('whoami').hidden = false;
  $('logout').hidden = false;
  for (const el of document.querySelectorAll('[data-admin]')) el.hidden = !state.me.is_admin;
  // Until the user picks their own password the API refuses everything else,
  // so only the password form is offered.
  const forced = state.me.must_change_password;
  $('tabs').hidden = forced;
  $('password-forced').hidden = !forced;
  route();
  if (state.me.is_admin && !forced) {
    // Shows pending requests on the admin link from any page.
    attempt(async () => showRequestCount((await api('/api/admin/reservation-requests')).length));
  }
}

const VIEWS = {
  overview: loadOverview,
  tunnels: loadTunnels,
  tokens: loadTokens,
  names: loadNames,
  traffic: startTraffic,
  account: () => {},
  admin: loadAdmin,
};

function route() {
  if (!state.me) return;
  let view = location.hash.slice(1);
  if (!VIEWS[view] || (view === 'admin' && !state.me.is_admin)) view = 'overview';
  if (state.me.must_change_password) view = 'account';
  stopViewWork();
  state.view = view;
  for (const section of document.querySelectorAll('[data-view]')) section.hidden = section.dataset.view !== view;
  for (const link of document.querySelectorAll('#tabs a')) {
    if (link.getAttribute('href') === `#${view}`) link.setAttribute('aria-current', 'page');
    else link.removeAttribute('aria-current');
  }
  attempt(VIEWS[view]);
}

// Polling and the live feed belong to one view; leaving it stops them.
function stopViewWork() {
  clearInterval(state.timer);
  state.timer = null;
  if (state.feed) {
    state.feed.close();
    state.feed = null;
  }
}

// --- Overview ----------------------------------------------------------------

function loadOverview() {
  const me = state.me;
  $('ov-email').textContent = me.email;
  $('ov-role').textContent = me.is_admin ? 'Administrator' : 'User';
  render('ov-subdomain', me.assigned_subdomain
    ? tunnelLink(me.assigned_subdomain)
    : h('span', { class: 'muted' }, 'None yet. One is assigned to you when you first need it.'));
  render('ov-quota', me.limits
    ? h('div', { class: 'stats' },
      stat('Tunnels at once', me.limits.max_tunnels),
      stat('Bandwidth', formatRate(me.limits.bandwidth_bytes_per_sec)),
      stat('Concurrent requests', me.limits.max_concurrent_requests))
    : h('div', { class: 'stats' }, stat('Quota', 'None'), stat('Role', 'Admin')));
  showCommand('ov-command', 'ov-command-note');
}
function stat(label, value) {
  return h('div', { class: 'card' }, h('div', { class: 'stat-value' }, value), h('div', { class: 'stat-label' }, label));
}

// --- Tunnels -----------------------------------------------------------------

async function loadTunnels() {
  await refreshTunnels();
  state.timer = setInterval(() => {
    if (!document.hidden) attempt(refreshTunnels);
  }, 5000);
}

async function refreshTunnels() {
  const tunnels = await api('/api/tunnels', { errors: { 503: 'Live tunnel state is unavailable right now.' } });
  const limits = state.me.limits;
  const count = tunnels.length;
  $('tunnels-count').textContent = limits
    ? `${count} of ${limits.max_tunnels} tunnels in use.` + (count >= limits.max_tunnels ? ' You are at your limit: the agent\'s next tunnel will be refused with TUNNEL_LIMIT.' : '')
    : `${count} tunnel${count === 1 ? '' : 's'} open. Administrators have no tunnel limit.`;
  render('tunnels-list', table(tunnels, [
    { label: 'URL', cell: (t) => tunnelLink(t.subdomain) },
    { label: 'Machine', cell: (t) => t.machine_id },
    { label: 'Upstream', cell: (t) => t.upstream_hint || '—' },
    { label: 'Active requests', cell: (t) => t.active_requests },
    { label: '', className: 'actions', cell: (t) => h('button', { type: 'button', class: 'danger small', onclick: (e) => closeTunnel(t.subdomain, e.target) }, 'Close') },
  ], 'No tunnels are open. Start the agent to open one.'));
}

async function closeTunnel(name, button) {
  if (!confirm(`Close the tunnel ${name}? Visitors lose access until the agent opens it again.`)) return;
  await attempt(async () => {
    const result = await api(`/api/tunnels/${encodeURIComponent(name)}/close`, { method: 'POST' });
    notify(result.ok ? `Closed ${name}.` : `${name} was already closed.`, true);
    await refreshTunnels();
  }, button);
}

// --- Tokens ------------------------------------------------------------------

async function loadTokens() {
  const [tokens, reservations] = await Promise.all([api('/api/tokens'), api('/api/reservations')]);
  state.reservations = reservations.filter((r) => r.status === 'reserved');
  renderAllowlistOptions();
  render('tokens-list', table(tokens, [
    { label: 'ID', cell: (t) => t.id },
    { label: 'May open', cell: (t) => t.bind_policy },
    { label: 'Allowed names', cell: (t) => t.allowlist.join(', ') || '—' },
    { label: 'Status', cell: (t) => t.revoked ? h('span', { class: 'badge bad' }, 'revoked') : h('span', { class: 'badge ok' }, 'active') },
    { label: '', className: 'actions', cell: (t) => t.revoked ? '' : h('button', { type: 'button', class: 'danger small', onclick: (e) => revokeToken(t.id, e.target) }, 'Revoke') },
  ], 'You have no tokens yet.'));
}

function renderAllowlistOptions() {
  render('token-allowlist-options', state.reservations.length
    ? h('div', {}, state.reservations.map((r) => h('label', { class: 'choice' },
      h('input', { type: 'checkbox', name: 'allowlist', value: r.name }), ' ', r.name)))
    : h('p', { class: 'empty' }, 'You have no approved reserved names. Reserve one on the ', h('a', { href: '#names' }, 'Reserved names'), ' page first.'));
}

function updatePolicyFields() {
  const reserved = $('token-form').elements.bind_policy.value === 'reserved';
  $('token-allowlist').hidden = !reserved;
}

async function createToken(event) {
  event.preventDefault();
  const form = event.target;
  const bindPolicy = form.elements.bind_policy.value;
  const allowlist = bindPolicy === 'reserved'
    ? [...form.querySelectorAll('input[name="allowlist"]:checked')].map((i) => i.value) : [];
  if (bindPolicy === 'reserved' && !allowlist.length) {
    notify('Pick at least one reserved name for a reserved token.');
    return;
  }
  await attempt(async () => {
    const body = { bind_policy: bindPolicy };
    if (allowlist.length) body.allowlist = allowlist;
    const result = await api('/api/tokens', { method: 'POST', body });
    $('token-raw').value = result.raw_token;
    showCommand('token-command', 'token-command-note', result.raw_token, allowlist);
    $('token-created').hidden = false;
    $('token-created').scrollIntoView({ behavior: 'smooth', block: 'start' });
    form.reset();
    updatePolicyFields();
    clearNotice();
    await loadTokens();
  }, form.querySelector('button[type="submit"]'));
}

function dismissToken() {
  $('token-raw').value = '';
  $('token-command').textContent = '';
  $('token-created').hidden = true;
}

async function revokeToken(id, button) {
  if (!confirm(`Revoke token ${id}? Agents using it are disconnected now and cannot reconnect.`)) return;
  await attempt(async () => {
    await api(`/api/tokens/${id}/revoke`, { method: 'POST', errors: {
      503: 'The token is revoked, but live agents could not be disconnected yet. Revoke again to retry.' } });
    notify(`Token ${id} revoked and its agents disconnected.`, true);
    await loadTokens();
  }, button);
}

// --- Reserved names ----------------------------------------------------------

async function loadNames() {
  const direct = state.me.can_reserve_directly;
  $('name-example').textContent = tunnelUrl('<name>');
  $('name-mode').textContent = direct
    ? 'Names you reserve are yours at once.'
    : 'An administrator approves each name you request. A pending request already holds the name, so nobody else can take it.';
  $('name-submit').textContent = direct ? 'Reserve' : 'Request';
  const names = await api('/api/reservations');
  render('names-list', table(names, [
    { label: 'Name', cell: (r) => r.name },
    { label: 'Status', cell: (r) => r.status === 'reserved'
      ? h('span', { class: 'badge ok' }, 'reserved')
      : h('span', { class: 'badge pending' }, 'awaiting approval') },
    { label: 'URL', cell: (r) => r.status === 'reserved' ? tunnelLink(r.name) : '—' },
    { label: '', className: 'actions', cell: (r) => h('button', { type: 'button', class: 'danger small', onclick: (e) => releaseName(r, e.target) }, r.status === 'reserved' ? 'Release' : 'Withdraw') },
  ], 'You have not reserved or requested any names.'));
}

async function reserveName(event) {
  event.preventDefault();
  const form = event.target;
  const name = form.elements.name.value.trim().toLowerCase();
  await attempt(async () => {
    // A 409 carries the relay's explanation of who holds the name.
    const result = await api('/api/reservations', { method: 'POST', body: { name } });
    form.reset();
    notify(result.status === 'reserved'
      ? `Reserved ${result.name}.`
      : `Requested ${result.name}. It is yours once an administrator approves it.`, true);
    await loadNames();
  }, $('name-submit'));
}

async function releaseName(reservation, button) {
  const { name, status } = reservation;
  const question = status === 'reserved'
    ? `Release ${name}? Anyone can reserve it afterwards.`
    : `Withdraw your request for ${name}?`;
  if (!confirm(question)) return;
  await attempt(async () => {
    await api(`/api/reservations/${encodeURIComponent(name)}`, { method: 'DELETE' });
    notify(status === 'reserved' ? `Released ${name}.` : `Withdrew the request for ${name}.`, true);
    await loadNames();
  }, button);
}

// --- Traffic -----------------------------------------------------------------

async function startTraffic() {
  renderTraffic(await api('/api/traffic/recent', { errors: { 503: 'Traffic is unavailable right now.' } }));
  openFeed();
}

function openFeed() {
  $('traffic-reconnect').hidden = true;
  setFeedStatus('Connecting', false);
  const feed = new EventSource('/api/traffic/stream');
  state.feed = feed;
  feed.onopen = () => setFeedStatus('Live', true);
  feed.onmessage = (event) => {
    try {
      renderTraffic(JSON.parse(event.data));
    } catch {
      setFeedStatus('Bad data from the relay', false);
    }
  };
  // Fires for connection errors and for the relay's own `error` events.
  feed.addEventListener('error', (event) => {
    if (event.data) {
      setFeedStatus('Traffic unavailable', false);
    } else if (feed.readyState === EventSource.CLOSED) {
      // The browser gave up: the session ended or the relay refused the stream (too many open).
      setFeedStatus('Disconnected', false);
      $('traffic-reconnect').hidden = false;
    } else {
      setFeedStatus('Reconnecting', false);
    }
  });
}

function setFeedStatus(text, live) {
  $('traffic-status').textContent = text;
  $('traffic-status').className = live ? 'badge ok' : 'badge';
}

function renderTraffic(events) {
  const sorted = [...events].sort((a, b) => b.timestamp_ms - a.timestamp_ms);
  render('traffic-list', table(sorted, [
    { label: 'Time', cell: (e) => new Date(e.timestamp_ms).toLocaleTimeString() },
    { label: 'Tunnel', cell: (e) => e.subdomain },
    { label: 'Method', cell: (e) => e.method },
    { label: 'Status', cell: (e) => e.status },
    { label: 'In', cell: (e) => formatBytes(e.bytes_in) },
    { label: 'Out', cell: (e) => formatBytes(e.bytes_out) },
  ], 'No requests yet. Traffic through your tunnels appears here.'));
}

async function reconnectFeed() {
  stopViewWork();
  // An ended session would only fail again; check it first so the user lands on login.
  await attempt(async () => {
    await api('/api/me');
    openFeed();
  }, $('traffic-reconnect'));
}

// --- Account -----------------------------------------------------------------

async function changePassword(event) {
  event.preventDefault();
  const form = event.target;
  const { old_password: oldPassword, new_password: newPassword, repeat_password: repeat } = formData(form);
  if (newPassword !== repeat) {
    notify('The new passwords do not match.');
    return;
  }
  const button = form.querySelector('button[type="submit"]');
  button.disabled = true;
  try {
    await api('/api/password', { method: 'POST', body: { old_password: oldPassword, new_password: newPassword },
      errors: { 401: 'The current password is wrong.' } });
    form.reset();
    await signedOut('Password changed. Log in with your new password.');
  } catch (error) {
    // A 401 here means a wrong current password, not a lost session.
    notify(error.message);
  } finally {
    button.disabled = false;
  }
}

// --- Admin -------------------------------------------------------------------

const LIMIT_FIELDS = ['max_tunnels', 'bandwidth_bytes_per_sec', 'max_concurrent_requests'];
const LIMITS_503 = 'Saved, but live tunnels have not picked it up yet. Save again to retry.';
let defaults = null;
let editing = null;

async function loadAdmin() {
  const [limits, users, requests] = await Promise.all([
    api('/api/admin/limits'), api('/api/admin/users'), api('/api/admin/reservation-requests')]);
  defaults = limits;
  showRequestCount(requests.length);
  renderRequests(requests);
  const form = $('defaults-form').elements;
  form.max_tunnels.value = limits.max_tunnels;
  form.bandwidth.value = bytesToMib(limits.bandwidth_bytes_per_sec);
  form.max_concurrent_requests.value = limits.max_concurrent_requests;
  renderUsers(users);
}

function renderUsers(users) {
  const cell = (field, format = (v) => v) => (u) => {
    if (!u.effective) return h('span', { class: 'muted' }, 'exempt');
    const inherited = u.overrides[field] == null;
    return h('span', {}, format(u.effective[field]), ' ', inherited ? h('span', { class: 'badge plain' }, 'default') : null);
  };
  render('users-list', table(users, [
    { label: 'Email', cell: (u) => h('span', {}, u.email, u.must_change_password ? h('span', {}, ' ', h('span', { class: 'badge pending' }, 'must set password')) : null) },
    { label: 'Role', cell: (u) => h('span', {},
      h('span', { class: u.is_admin ? 'badge ok' : 'badge' }, u.is_admin ? 'admin' : 'user'), ' ',
      u.id === state.me.id
        ? h('span', { class: 'muted' }, '(you)')
        : h('button', { type: 'button', class: 'small', onclick: (e) => setAccess(u, { is_admin: !u.is_admin }, e.target) }, u.is_admin ? 'Make user' : 'Make admin')) },
    { label: 'Direct', cell: (u) => u.is_admin
      ? h('span', { class: 'muted' }, 'always')
      : h('input', { type: 'checkbox', 'aria-label': `Direct reservation for ${u.email}`, checked: u.can_reserve_directly, onchange: (e) => setAccess(u, { can_reserve_directly: e.target.checked }, e.target) }) },
    { label: 'Tunnels', cell: cell('max_tunnels') },
    { label: 'Bandwidth', cell: cell('bandwidth_bytes_per_sec', formatRate) },
    { label: 'Requests', cell: cell('max_concurrent_requests') },
    { label: '', className: 'actions', cell: (u) => u.is_admin ? '' : h('button', { type: 'button', class: 'small', onclick: () => openLimits(u) }, 'Edit quota') },
  ], 'No users.'));
}

function showRequestCount(count) {
  $('admin-count').textContent = count;
  $('admin-count').hidden = !count;
}

function renderRequests(requests) {
  render('requests-list', table(requests, [
    { label: 'Name', cell: (r) => r.name },
    { label: 'Requested by', cell: (r) => r.email },
    { label: 'When', cell: (r) => new Date(r.created_at_ms).toLocaleString() },
    { label: '', className: 'actions', cell: (r) => h('span', {},
      h('button', { type: 'button', class: 'primary small', onclick: (e) => decideRequest(r, 'approve', e.target) }, 'Approve'),
      h('button', { type: 'button', class: 'danger small', onclick: (e) => decideRequest(r, 'reject', e.target) }, 'Reject')) },
  ], 'No pending requests.'));
}

async function decideRequest(request, decision, button) {
  if (decision === 'reject' && !confirm(`Reject ${request.email}'s request for ${request.name}? The name becomes free.`)) return;
  await attempt(async () => {
    await api(`/api/admin/reservation-requests/${encodeURIComponent(request.name)}/${decision}`, { method: 'POST' });
    notify(decision === 'approve'
      ? `${request.name} is now reserved for ${request.email}.`
      : `Rejected the request for ${request.name}.`, true);
    await loadAdmin();
  }, button);
}

// The PUT replaces both fields, so start from the user's current values.
async function setAccess(user, change, control) {
  const body = { is_admin: user.is_admin, can_reserve_directly: user.can_reserve_directly, ...change };
  if (change.is_admin !== undefined && !confirm(change.is_admin
    ? `Make ${user.email} an administrator? Admins manage every user and have no quota.`
    : `Remove ${user.email}'s admin role? Their quota applies again at once.`)) return;
  const ok = await attempt(async () => {
    await api(`/api/admin/users/${user.id}`, { method: 'PUT', body, errors: { 503: LIMITS_503 } });
    notify(`Updated ${user.email}.`, true);
    return true;
  }, control);
  // Reload either way so a failed toggle shows the stored value again.
  await attempt(loadAdmin);
  return ok;
}

// Reads a positive integer field; returns null and reports when invalid.
function positive(value, label) {
  const n = Number(value);
  if (!Number.isSafeInteger(n) || n < 1) {
    notify(`${label} must be a whole number of at least 1.`);
    return null;
  }
  return n;
}
function bandwidth(value) {
  const bytes = mibToBytes(value);
  if (!Number.isSafeInteger(bytes) || bytes < 1) {
    notify('Bandwidth must be more than 0 MiB/s.');
    return null;
  }
  return bytes;
}

async function saveDefaults(event) {
  event.preventDefault();
  const form = event.target.elements;
  const body = {
    max_tunnels: positive(form.max_tunnels.value, 'Max tunnels'),
    bandwidth_bytes_per_sec: bandwidth(form.bandwidth.value),
    max_concurrent_requests: positive(form.max_concurrent_requests.value, 'Concurrent requests'),
  };
  if (Object.values(body).includes(null)) return;
  await attempt(async () => {
    await api('/api/admin/limits', { method: 'PUT', body, errors: { 503: LIMITS_503 } });
    notify('Default quotas saved.', true);
    await loadAdmin();
  }, event.target.querySelector('button[type="submit"]'));
}

function openLimits(user) {
  editing = user;
  $('limits-email').textContent = user.email;
  for (const row of document.querySelectorAll('#limits-form .override')) {
    const field = row.dataset.field;
    const [input, box] = row.querySelectorAll('input');
    const isRate = field === 'bandwidth_bytes_per_sec';
    const toInput = isRate ? bytesToMib : (v) => v;
    const override = user.overrides[field];
    row.querySelector('span').textContent = isRate ? formatRate(defaults[field]) : defaults[field];
    box.checked = override == null;
    input.value = toInput(override == null ? defaults[field] : override);
    input.disabled = box.checked;
    box.onchange = () => { input.disabled = box.checked; };
  }
  $('limits-dialog').showModal();
}

async function saveLimits(event) {
  event.preventDefault();
  // The PUT replaces every field, so always send all three: null means "use the default".
  const body = {};
  for (const row of document.querySelectorAll('#limits-form .override')) {
    const field = row.dataset.field;
    const [input, box] = row.querySelectorAll('input');
    if (box.checked) {
      body[field] = null;
      continue;
    }
    const label = row.querySelector('label').firstChild.textContent.trim();
    body[field] = field === 'bandwidth_bytes_per_sec' ? bandwidth(input.value) : positive(input.value, label);
    if (body[field] === null) return;
  }
  await attempt(async () => {
    await api(`/api/admin/users/${editing.id}/limits`, { method: 'PUT', body, errors: { 503: LIMITS_503 } });
    $('limits-dialog').close();
    notify(`Quota for ${editing.email} saved.`, true);
    await loadAdmin();
  }, $('limits-save'));
}

async function createUser(event) {
  event.preventDefault();
  const form = event.target;
  const { email, password } = formData(form);
  await attempt(async () => {
    const user = await api('/api/users', { method: 'POST', body: { email, password },
      errors: { 409: 'That email is already registered.' } });
    form.reset();
    notify(`Created ${user.email}. Send them the password privately; they must change it at first login.`, true);
    await loadAdmin();
  }, form.querySelector('button[type="submit"]'));
}

// --- Startup -----------------------------------------------------------------

function wire() {
  $('notice-close').addEventListener('click', clearNotice);
  for (const button of document.querySelectorAll('[data-theme-toggle]')) button.addEventListener('click', toggleTheme);
  $('logout').addEventListener('click', logout);
  $('auth-form').addEventListener('submit', submitAuth);
  $('auth-toggle').addEventListener('click', () => { clearNotice(); setAuthMode(authMode === 'login' ? 'signup' : 'login'); });
  window.addEventListener('hashchange', route);
  $('tunnels-refresh').addEventListener('click', (e) => attempt(refreshTunnels, e.target));
  $('token-form').addEventListener('submit', createToken);
  $('token-form').addEventListener('change', updatePolicyFields);
  $('token-done').addEventListener('click', dismissToken);
  $('name-form').addEventListener('submit', reserveName);
  $('traffic-reconnect').addEventListener('click', reconnectFeed);
  $('password-form').addEventListener('submit', changePassword);
  $('defaults-form').addEventListener('submit', saveDefaults);
  $('limits-form').addEventListener('submit', saveLimits);
  $('limits-cancel').addEventListener('click', () => $('limits-dialog').close());
  $('user-form').addEventListener('submit', createUser);
  for (const button of document.querySelectorAll('[data-copy]')) {
    button.addEventListener('click', () => {
      const source = $(button.dataset.copy);
      copy(source.value ?? source.textContent, button);
    });
  }
}

async function start() {
  wire();
  try {
    state.config = await api('/api/config');
  } catch (error) {
    notify(`Cannot load the relay settings: ${error.message}`);
    return;
  }
  try {
    await enterApp();
  } catch (error) {
    if (error.status !== 401) notify(error.message);
    showAuth();
  }
}

document.addEventListener('DOMContentLoaded', start);
