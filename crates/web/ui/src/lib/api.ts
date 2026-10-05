// Typed client for the relay's dashboard API (docs/api.md).

export type Config = { signup_mode: 'open' | 'closed'; needs_bootstrap: boolean; base_domain: string };
export type Limits = { max_tunnels: number; bandwidth_bytes_per_sec: number; max_concurrent_requests: number };
export type LimitField = keyof Limits;
export type Me = {
  id: number;
  email: string;
  is_admin: boolean;
  assigned_subdomain: string | null;
  limits: Limits | null;
  must_change_password: boolean;
  can_reserve_directly: boolean;
};
export type BindPolicy = 'any' | 'temporary' | 'reserved';
export type Token = { id: number; bind_policy: BindPolicy; allowlist: string[]; revoked: boolean };
export type Reservation = { name: string; status: 'reserved' | 'pending' };
export type Tunnel = { subdomain: string; machine_id: string; upstream_hint: string | null; active_requests: number };
export type TrafficEvent = {
  subdomain: string;
  timestamp_ms: number;
  method: string;
  status: number;
  bytes_in: number;
  bytes_out: number;
};
export type AdminUser = {
  id: number;
  email: string;
  is_admin: boolean;
  created_at_ms: number;
  must_change_password: boolean;
  can_reserve_directly: boolean;
  overrides: Record<LimitField, number | null>;
  effective: Limits | null;
};
export type ReservationRequest = { name: string; user_id: number; email: string; created_at_ms: number };

export class ApiError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
  }
}

const FALLBACK: Record<number, string> = {
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

// The relay's error strings are terse codes; spell out the ones that need it.
const KNOWN: Record<string, string> = {
  unauthorized: FALLBACK[401],
  forbidden: FALLBACK[403],
  conflict: FALLBACK[409],
  'rate limit exceeded': FALLBACK[429],
  'relay disconnect unavailable': FALLBACK[503],
  'internal error': FALLBACK[500],
};
export const PASSWORD_CHANGE_REQUIRED = 'Password change required.';

function explain(error: unknown): string {
  if (typeof error !== 'string' || !error) return '';
  return KNOWN[error] ?? error.charAt(0).toUpperCase() + error.slice(1) + '.';
}

type Options = { method?: 'GET' | 'POST' | 'PUT' | 'DELETE'; body?: unknown; errors?: Record<number, string> };

/** `errors` maps a status code to a message that explains it for this call. */
export async function api<T>(path: string, { method = 'GET', body, errors = {} }: Options = {}): Promise<T> {
  const headers: Record<string, string> = {};
  if (method !== 'GET') headers['x-vorp-csrf'] = '1';
  if (body !== undefined) headers['content-type'] = 'application/json';
  let response: Response;
  try {
    response = await fetch(path, {
      method,
      headers,
      credentials: 'same-origin',
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch {
    throw new ApiError(0, FALLBACK[0]);
  }
  const isJson = (response.headers.get('content-type') ?? '').includes('application/json');
  const data = isJson ? await response.json().catch(() => null) : null;
  if (response.ok) return data as T;
  const text = isJson ? '' : (await response.text().catch(() => '')).trim().slice(0, 200);
  const status = response.status;
  // Specific explanation first, then the server's message, then a generic one.
  const message = errors[status] || explain(data?.error) || text || FALLBACK[status] || `Request failed (HTTP ${status}).`;
  const retry = response.headers.get('retry-after');
  throw new ApiError(status, retry && status === 429 ? `${message} (retry in ${retry}s)` : message);
}

export const post = <T>(path: string, body?: unknown, errors?: Record<number, string>) =>
  api<T>(path, { method: 'POST', body, errors });
