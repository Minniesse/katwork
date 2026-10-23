import { hasMessage, t } from './i18n';
import type { ApiMessage } from './sse';

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

export type Role = 'owner' | 'admin' | 'member';

export interface WorkspaceSummary {
  id: string;
  name: string;
  role: Role;
  paused: boolean;
}

export interface Space {
  id: string;
  name: string;
}

export interface Team {
  id: string;
  space_id: string;
  name: string;
}

/** `KatRole`: a snake_case name, or `{"custom": "<name>"}`. */
export type KatRole = string | { custom: string };

export interface Kat {
  id: string;
  name: string;
  role: KatRole;
  status: 'active' | 'paused';
  appearance: { avatar: string; accent: string };
}

/** A team with its active members, the lead first (Task 5). */
export interface TeamView {
  team: Team;
  members: Kat[];
}

export interface Thread {
  id: string;
  space_id: string;
  team_id: string;
  title: string;
}

export interface MessagePage {
  messages: ApiMessage[];
  event_seq: number;
}

export interface Posted {
  message_id: string;
  job_id: string | null;
}

/** Task 12's `JobView`; `queue_position` is set only while the job's task waits in the queue. */
export interface JobView {
  id: string;
  status: string | null;
  reason: string | null;
  hold_reason: string | null;
  queue_position: number | null;
}

export interface Member {
  user_id: string;
  email: string;
  display_name: string;
  role: Role;
}

export type InviteDelivery = 'email' | 'link';

export interface InviteResult {
  user_id: string;
  created: boolean;
  link: string | null;
}

const workspacePath = (workspaceId: string) => `/api/workspaces/${encodeURIComponent(workspaceId)}`;
const segment = encodeURIComponent;

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
  workspaces: () => apiFetch<WorkspaceSummary[]>('/api/workspaces'),
  spaces: (workspaceId: string) => apiFetch<Space[]>(`${workspacePath(workspaceId)}/spaces`),
  teams: (workspaceId: string, spaceId: string) =>
    apiFetch<Team[]>(`${workspacePath(workspaceId)}/spaces/${segment(spaceId)}/teams`),
  team: (workspaceId: string, teamId: string) =>
    apiFetch<TeamView>(`${workspacePath(workspaceId)}/teams/${segment(teamId)}`),
  threads: (workspaceId: string, spaceId: string) =>
    apiFetch<Thread[]>(`${workspacePath(workspaceId)}/spaces/${segment(spaceId)}/threads`),
  createThread: (workspaceId: string, thread: { space_id: string; team_id: string; title: string }) =>
    apiFetch<Thread>(`${workspacePath(workspaceId)}/threads`, { method: 'POST', body: thread }),
  thread: (workspaceId: string, threadId: string) =>
    apiFetch<Thread>(`${workspacePath(workspaceId)}/threads/${segment(threadId)}`),
  messages: (workspaceId: string, threadId: string) =>
    apiFetch<MessagePage>(`${workspacePath(workspaceId)}/threads/${segment(threadId)}/messages`),
  postMessage: (workspaceId: string, threadId: string, body: string, clientMsgId: string) =>
    apiFetch<Posted>(`${workspacePath(workspaceId)}/threads/${segment(threadId)}/messages`, {
      method: 'POST',
      body: { body, client_msg_id: clientMsgId },
    }),
  /** The thread's event stream path, without the `?after=` cursor `subscribe` adds. */
  eventsPath: (workspaceId: string, threadId: string) =>
    `${workspacePath(workspaceId)}/threads/${segment(threadId)}/events`,
  job: (workspaceId: string, jobId: string) =>
    apiFetch<JobView>(`${workspacePath(workspaceId)}/jobs/${segment(jobId)}`),
  cancelJob: (workspaceId: string, jobId: string) =>
    apiFetch<unknown>(`${workspacePath(workspaceId)}/jobs/${segment(jobId)}/cancel`, {
      method: 'POST',
    }).then(() => undefined),
  members: (workspaceId: string) => apiFetch<Member[]>(`${workspacePath(workspaceId)}/members`),
  invite: (workspaceId: string, invite: { email: string; role: Role; delivery: InviteDelivery }) =>
    apiFetch<InviteResult>(`${workspacePath(workspaceId)}/members`, { method: 'POST', body: invite }),
  setRole: (workspaceId: string, userId: string, role: Role) =>
    apiFetch<unknown>(`${workspacePath(workspaceId)}/members/${segment(userId)}/role`, {
      method: 'PUT',
      body: { role },
    }).then(() => undefined),
  removeMember: (workspaceId: string, userId: string) =>
    apiFetch<unknown>(`${workspacePath(workspaceId)}/members/${segment(userId)}`, {
      method: 'DELETE',
    }).then(() => undefined),
};
