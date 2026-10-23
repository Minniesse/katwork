import { hasMessage, t } from './i18n';

/** An API failure. `code` is the server's stable id, or `network_error` / `unknown` from the client. */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
  ) {
    super(code);
    this.name = 'ApiError';
  }
}

export interface AuthUser {
  user_id: string;
}

type Method = 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE';

const CODE = /^[a-z][a-z0-9_]{0,63}$/;

/**
 * Calls the KatWork API. Only same-origin `/api/` paths are allowed, so the session cookie
 * never leaves the origin; the browser adds the `Origin` and `Sec-Fetch-Site` headers the
 * server's guards check. Bodies are always JSON.
 */
export async function apiFetch<T>(path: string, options: { method?: Method; body?: unknown } = {}): Promise<T> {
  const url = new URL(path, window.location.origin);
  if (
    !path.startsWith('/api/') ||
    url.origin !== window.location.origin ||
    !url.pathname.startsWith('/api/')
  ) {
    throw new ApiError(0, 'request_invalid');
  }
  const method = options.method ?? 'GET';
  const headers = new Headers({ accept: 'application/json' });
  let body: string | undefined;
  if (method !== 'GET') {
    headers.set('content-type', 'application/json');
    body = JSON.stringify(options.body ?? {});
  }
  let response: Response;
  try {
    response = await fetch(path, {
      method,
      headers,
      body,
      credentials: 'same-origin',
      mode: 'same-origin',
      cache: 'no-store',
      redirect: 'error',
    });
  } catch {
    throw new ApiError(0, 'network_error');
  }
  const data: unknown = await response.json().catch(() => undefined);
  if (!response.ok) {
    const code = (data as { code?: unknown } | undefined)?.code;
    throw new ApiError(response.status, typeof code === 'string' && CODE.test(code) ? code : 'unknown');
  }
  return data as T;
}

/** The localized `reason.*` message for any error thrown by the API helpers. */
export function errorMessage(error: unknown): string {
  const code = error instanceof ApiError ? error.code : 'unknown';
  const key = `reason.${code}`;
  return t(CODE.test(code) && hasMessage(key) ? key : 'reason.unknown');
}

export const api = {
  login: (email: string, password: string) =>
    apiFetch<AuthUser>('/api/auth/login', { method: 'POST', body: { email, password } }),
  logout: () => apiFetch<unknown>('/api/auth/logout', { method: 'POST' }).then(() => undefined),
  me: () => apiFetch<AuthUser>('/api/auth/me'),
  requestReset: (email: string) =>
    apiFetch<unknown>('/api/auth/reset/request', { method: 'POST', body: { email } }).then(
      () => undefined,
    ),
  confirmReset: (token: string, newPassword: string) =>
    apiFetch<unknown>('/api/auth/reset/confirm', {
      method: 'POST',
      body: { token, new_password: newPassword },
    }).then(() => undefined),
  acceptInvite: (token: string, newPassword: string) =>
    apiFetch<unknown>('/api/auth/invite/accept', {
      method: 'POST',
      body: { token, new_password: newPassword },
    }).then(() => undefined),
};
