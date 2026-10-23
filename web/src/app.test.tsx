import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { App } from './app';
import { setLocale } from './i18n';
import en from './i18n/en.json';
import th from './i18n/th.json';
import { navigate } from './router';
import { apiMessage } from './testing/agui';
import { mockApi, type Reply } from './testing/api-mock';
import { FakeEventSource, installFakeEventSource } from './testing/fake-event-source';
import { stubFetch } from './testing/fetch';
import { ME, SPACE, TEAM, THAI_TEAM, WORKSPACE, WORKSPACES } from './testing/fixtures';

beforeEach(() => {
  window.history.replaceState(null, '', '/');
  installFakeEventSource();
});

const BASE = `/api/workspaces/${WORKSPACE}`;

/** A signed-in person with one workspace, one Space and the Thai default team. */
function signedIn(routes: Record<string, Reply | Reply[]> = {}) {
  return mockApi({
    'GET /api/auth/me': [200, { user_id: ME }],
    'GET /api/workspaces': [200, WORKSPACES],
    [`GET ${BASE}/spaces`]: [200, [{ id: SPACE, name: 'พื้นที่ทดลอง' }]],
    [`GET ${BASE}/spaces/${SPACE}/teams`]: [200, [THAI_TEAM.team]],
    [`GET ${BASE}/teams/${TEAM}`]: [200, THAI_TEAM],
    [`GET ${BASE}/spaces/${SPACE}/threads`]: [200, []],
    ...routes,
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
  act(() => setLocale('en'));
});

test('signed_out_visitor_is_sent_to_login', async () => {
  stubFetch([401, { code: 'unauthenticated' }]);
  render(<App />);
  expect(await screen.findByRole('heading', { level: 1, name: en['login.title'] })).toBeVisible();
  expect(window.location.pathname).toBe('/login');
});

test('signed_in_home_shows_team_and_chat', async () => {
  signedIn();
  render(<App />);
  expect(await screen.findByRole('heading', { level: 1, name: en['home.title'] })).toBeVisible();
  const team = screen.getByRole('region', { name: en['home.team.title'] });
  expect(await within(team).findByRole('heading', { name: 'หัวหน้าทีม' })).toBeVisible();
  const chat = screen.getByRole('region', { name: en['home.chat.title'] });
  expect(await within(chat).findByText(en['chat.no_threads'])).toBeVisible();
});

test('login_then_home', async () => {
  window.history.replaceState(null, '', '/login');
  signedIn({ 'GET /api/auth/me': [401, { code: 'unauthenticated' }], 'POST /api/auth/login': [200, { user_id: ME }] });
  const user = userEvent.setup({ delay: null });
  render(<App />);
  await user.type(await screen.findByLabelText(en['login.email']), 'person@example.test');
  await user.type(screen.getByLabelText(en['login.password']), 'correct horse');
  await user.click(screen.getByRole('button', { name: en['login.submit'] }));
  expect(await screen.findByRole('heading', { level: 1, name: en['home.title'] })).toBeVisible();
  expect(window.location.pathname).toBe('/');
});

test('sign_out_returns_to_login', async () => {
  const calls = signedIn({ 'POST /api/auth/logout': [200, {}] });
  const user = userEvent.setup({ delay: null });
  render(<App />);
  await user.click(await screen.findByRole('button', { name: en['nav.sign_out'] }));
  expect(await screen.findByRole('heading', { level: 1, name: en['login.title'] })).toBeVisible();
  expect(calls.callsTo('POST /api/auth/logout')).toHaveLength(1);
});

test('failed_sign_out_keeps_the_session_and_can_be_retried', async () => {
  const calls = signedIn({
    'POST /api/auth/logout': [[503, { code: 'database_unavailable' }], [200, {}]],
  });
  const user = userEvent.setup({ delay: null });
  render(<App />);
  await screen.findByRole('heading', { level: 1, name: en['home.title'] });
  await user.click(screen.getByRole('button', { name: en['nav.sign_out'] }));

  // The server clears the cookie only after it revokes the session, so nothing may suggest
  // the person is signed out.
  const alert = await screen.findByRole('alert');
  expect(alert).toHaveTextContent(en['nav.sign_out_failed']);
  expect(alert).toHaveTextContent(en['reason.database_unavailable']);
  expect(screen.getByRole('heading', { level: 1, name: en['home.title'] })).toBeVisible();
  expect(screen.queryByRole('heading', { name: en['login.title'] })).toBeNull();
  expect(window.location.pathname).toBe('/');

  await user.click(screen.getByRole('button', { name: en['nav.sign_out'] }));
  expect(await screen.findByRole('heading', { level: 1, name: en['login.title'] })).toBeVisible();
  expect(screen.queryByRole('alert')).toBeNull();
  expect(calls.callsTo('POST /api/auth/logout')).toHaveLength(2);
});

test('chat_lists_threads_newest_first_and_reopens_one_after_reload', async () => {
  window.history.replaceState(null, '', '/chat');
  const older = { id: '0190a000-0000-7000-8000-000000000001', space_id: SPACE, team_id: TEAM, title: 'งบประมาณ' };
  const newer = { id: '0190a000-0000-7000-8000-000000000002', space_id: SPACE, team_id: TEAM, title: '' };
  const routes = {
    [`GET ${BASE}/spaces/${SPACE}/threads`]: [200, [newer, older]] as Reply,
    [`GET ${BASE}/threads/${older.id}`]: [200, older] as Reply,
    [`GET ${BASE}/threads/${older.id}/messages`]: [
      200,
      { messages: [apiMessage('01-ask', 'ขอสรุปงบ', { author: { kind: 'user', id: ME } })], event_seq: 4 },
    ] as Reply,
  };
  signedIn(routes);
  const user = userEvent.setup({ delay: null });
  const { unmount } = render(<App />);
  expect(await screen.findByRole('heading', { level: 1, name: en['chat.title'] })).toBeVisible();
  const list = await screen.findByRole('list', { name: en['chat.title'] });
  const links = within(list).getAllByRole('link');
  expect(links.map((link) => link.textContent)).toEqual([en['chat.untitled'], 'งบประมาณ']);

  await user.click(links[1]!);
  expect(window.location.pathname).toBe(`/chat/${older.id}`);
  expect(await screen.findByRole('heading', { level: 1, name: 'งบประมาณ' })).toBeVisible();
  expect(await screen.findByText('ขอสรุปงบ')).toBeVisible();

  // A reload of the same address opens the same thread from the server.
  unmount();
  installFakeEventSource();
  signedIn(routes);
  render(<App />);
  expect(await screen.findByRole('heading', { level: 1, name: 'งบประมาณ' })).toBeVisible();
  expect(await screen.findByText('ขอสรุปงบ')).toBeVisible();
  await waitFor(() => expect(FakeEventSource.latest().url).toBe(`${BASE}/threads/${older.id}/events?after=4`));
});

test('new_conversation_is_created_with_the_default_team', async () => {
  window.history.replaceState(null, '', '/chat');
  const created = { id: 'thread-new', space_id: SPACE, team_id: TEAM, title: 'แผนงาน' };
  const calls = signedIn({
    [`POST ${BASE}/threads`]: [201, created],
    [`GET ${BASE}/threads/thread-new`]: [200, created],
    [`GET ${BASE}/threads/thread-new/messages`]: [200, { messages: [], event_seq: 0 }],
  });
  const user = userEvent.setup({ delay: null });
  render(<App />);
  await user.type(await screen.findByLabelText(en['chat.new_title']), 'แผนงาน');
  await user.click(screen.getByRole('button', { name: en['chat.create'] }));
  expect(calls.callsTo(`POST ${BASE}/threads`)[0]!.body).toEqual({ space_id: SPACE, team_id: TEAM, title: 'แผนงาน' });
  expect(await screen.findByRole('heading', { level: 1, name: 'แผนงาน' })).toBeVisible();
  expect(window.location.pathname).toBe('/chat/thread-new');
});

test('closed_stream_with_401_routes_to_login', async () => {
  window.history.replaceState(null, '', '/chat/thread-1');
  signedIn({
    'GET /api/auth/me': [[200, { user_id: ME }], [401, { code: 'unauthenticated' }]],
    [`GET ${BASE}/threads/thread-1`]: [200, { id: 'thread-1', space_id: SPACE, team_id: TEAM, title: 'x' }],
    [`GET ${BASE}/threads/thread-1/messages`]: [200, { messages: [], event_seq: 0 }],
  });
  render(<App />);
  await waitFor(() => expect(FakeEventSource.live()).toHaveLength(1));
  act(() => FakeEventSource.latest().fail());
  expect(await screen.findByRole('heading', { level: 1, name: en['login.title'] })).toBeVisible();
  expect(window.location.pathname).toBe('/login');
});

test('team_page_route_shows_the_spaces_team', async () => {
  window.history.replaceState(null, '', '/team');
  signedIn();
  render(<App />);
  expect(await screen.findByRole('heading', { level: 1, name: en['team.title'] })).toBeVisible();
  expect(await screen.findByRole('heading', { level: 3, name: 'นักวิจัย' })).toBeVisible();
});

test('workspace_picker_switches_workspace', async () => {
  const thai = { id: 'ws-2', name: 'เดโม', role: 'member', paused: false };
  const english = { id: WORKSPACE, name: 'KatWork demo', role: 'owner', paused: false };
  signedIn({
    'GET /api/workspaces': [200, [english, thai]],
    'GET /api/workspaces/ws-2/spaces': [200, [{ id: 'space-2', name: 'พื้นที่ทดลอง' }]],
    'GET /api/workspaces/ws-2/spaces/space-2/teams': [200, []],
    'GET /api/workspaces/ws-2/spaces/space-2/threads': [200, []],
  });
  const user = userEvent.setup({ delay: null });
  render(<App />);
  const picker = await screen.findByRole('combobox', { name: en['nav.workspace'] });
  expect(screen.getByRole('link', { name: en['nav.members'] })).toBeVisible();
  await user.selectOptions(picker, 'ws-2');
  await waitFor(() => expect(screen.queryByRole('link', { name: en['nav.members'] })).toBeNull());
  expect(localStorage.getItem('kw-workspace')).toBe('ws-2');
});

test('reset_link_opens_confirm_and_hides_token_from_address_bar', async () => {
  window.history.replaceState(null, '', '/reset?token=abc123');
  stubFetch([401, { code: 'unauthenticated' }]);
  render(<App />);
  expect(
    await screen.findByRole('heading', { level: 1, name: en['reset.confirm.title'] }),
  ).toBeVisible();
  expect(window.location.search).toBe('');
  expect(window.location.pathname).toBe('/reset');
  expect(window.history.state).toBeNull();
});

test('request_again_link_from_a_used_reset_link_opens_the_request_form', async () => {
  window.history.replaceState(null, '', '/reset?token=used');
  stubFetch([401, { code: 'unauthenticated' }], [400, { code: 'reset_token_invalid' }]);
  const user = userEvent.setup({ delay: null });
  render(<App />);
  await user.type(await screen.findByLabelText(en['reset.confirm.password']), 'a new long password');
  await user.type(screen.getByLabelText(en['reset.confirm.again']), 'a new long password');
  await user.click(screen.getByRole('button', { name: en['reset.confirm.submit'] }));
  await user.click(await screen.findByRole('link', { name: en['reset.request_again'] }));
  expect(
    await screen.findByRole('heading', { level: 1, name: en['reset.request.title'] }),
  ).toBeVisible();
});

test('reset_while_signed_in_rechecks_the_session_before_sign_in', async () => {
  window.history.replaceState(null, '', '/reset?token=abc123');
  // Signed in when the link opens; the reset revokes every session, so the re-check is 401.
  const fetchMock = stubFetch(
    [200, { user_id: 'user-1' }],
    [200, {}],
    [401, { code: 'unauthenticated' }],
  );
  const user = userEvent.setup({ delay: null });
  render(<App />);
  await user.type(await screen.findByLabelText(en['reset.confirm.password']), 'a new long password');
  await user.type(screen.getByLabelText(en['reset.confirm.again']), 'a new long password');
  await user.click(screen.getByRole('button', { name: en['reset.confirm.submit'] }));
  await user.click(await screen.findByRole('link', { name: en['reset.back_to_login'] }));

  expect(await screen.findByRole('heading', { level: 1, name: en['login.title'] })).toBeVisible();
  expect(window.location.pathname).toBe('/login');
  expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
    '/api/auth/me',
    '/api/auth/reset/confirm',
    '/api/auth/me',
  ]);
});

test('invite_link_opens_accept_page_and_hides_token_from_address_bar', async () => {
  window.history.replaceState(null, '', '/invite?token=invite-token');
  const fetchMock = stubFetch([401, { code: 'unauthenticated' }], [204], [401, { code: 'unauthenticated' }]);
  const user = userEvent.setup({ delay: null });
  render(<App />);
  expect(
    await screen.findByRole('heading', { level: 1, name: en['invite.title'] }),
  ).toBeVisible();
  // Signed out, the invite page stays: it is not sent to sign in.
  expect(window.location.pathname).toBe('/invite');
  expect(window.location.search).toBe('');
  await user.type(screen.getByLabelText(en['invite.password']), 'a new long password');
  await user.type(screen.getByLabelText(en['invite.again']), 'a new long password');
  await user.click(screen.getByRole('button', { name: en['invite.submit'] }));
  await user.click(await screen.findByRole('link', { name: en['reset.back_to_login'] }));
  expect(await screen.findByRole('heading', { level: 1, name: en['login.title'] })).toBeVisible();
  expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
    '/api/auth/me',
    '/api/auth/invite/accept',
    '/api/auth/me',
  ]);
});

/** Goes back one history entry and waits until the page has handled it. */
async function goBack() {
  await act(async () => {
    const popped = new Promise((resolve) =>
      window.addEventListener('popstate', resolve, { once: true }),
    );
    window.history.back();
    await popped;
  });
}

test('invite_token_is_gone_after_leaving_and_coming_back', async () => {
  window.history.replaceState(null, '', '/invite?token=invite-token');
  const fetchMock = stubFetch([401, { code: 'unauthenticated' }]);
  render(<App />);
  await screen.findByRole('heading', { level: 1, name: en['invite.title'] });
  expect(window.location.href).not.toContain('invite-token');
  expect(window.history.state).toBeNull();

  act(() => navigate('/login'));
  await screen.findByRole('heading', { level: 1, name: en['login.title'] });
  await goBack();
  expect(window.location.pathname).toBe('/invite');
  expect(window.location.href).not.toContain('invite-token');
  expect(window.history.state).toBeNull();
  expect(await screen.findByRole('alert')).toHaveTextContent(en['invite.no_token']);
  expect(screen.queryByLabelText(en['invite.password'])).toBeNull();
  expect(fetchMock.mock.calls.map(([url]) => url)).not.toContain('/api/auth/invite/accept');
});

test('reset_token_is_gone_after_leaving_and_coming_back', async () => {
  window.history.replaceState(null, '', '/reset?token=reset-token');
  stubFetch([401, { code: 'unauthenticated' }]);
  render(<App />);
  await screen.findByRole('heading', { level: 1, name: en['reset.confirm.title'] });
  expect(window.history.state).toBeNull();

  act(() => navigate('/login'));
  await screen.findByRole('heading', { level: 1, name: en['login.title'] });
  await goBack();
  expect(window.location.pathname).toBe('/reset');
  expect(window.location.href).not.toContain('reset-token');
  expect(window.history.state).toBeNull();
  // Without its token the page is the request form, not the confirm form.
  expect(
    await screen.findByRole('heading', { level: 1, name: en['reset.request.title'] }),
  ).toBeVisible();
});

test.each([
  ['pagehide', false],
  ['pageshow', true],
] as const)('reset_token_is_dropped_on_%s', async (type, persisted) => {
  window.history.replaceState(null, '', '/reset?token=reset-token');
  stubFetch([401, { code: 'unauthenticated' }]);
  render(<App />);
  await screen.findByRole('heading', { level: 1, name: en['reset.confirm.title'] });
  act(() => {
    window.dispatchEvent(new PageTransitionEvent(type, { persisted }));
  });
  expect(
    await screen.findByRole('heading', { level: 1, name: en['reset.request.title'] }),
  ).toBeVisible();
  expect(screen.queryByLabelText(en['reset.confirm.password'])).toBeNull();
});

test('reset_without_token_opens_the_request_form', async () => {
  window.history.replaceState(null, '', '/reset');
  stubFetch([401, { code: 'unauthenticated' }]);
  render(<App />);
  expect(
    await screen.findByRole('heading', { level: 1, name: en['reset.request.title'] }),
  ).toBeVisible();
});

test('locale_switch_changes_copy_and_persists', async () => {
  window.history.replaceState(null, '', '/login');
  stubFetch([401, { code: 'unauthenticated' }]);
  const user = userEvent.setup({ delay: null });
  render(<App />);
  await screen.findByRole('heading', { level: 1, name: en['login.title'] });
  await user.click(screen.getByRole('button', { name: 'ไทย' }));
  expect(screen.getByRole('heading', { level: 1, name: th['login.title'] })).toBeVisible();
  expect(screen.getByRole('button', { name: 'ไทย' })).toHaveAttribute('aria-pressed', 'true');
  expect(localStorage.getItem('kw-locale')).toBe('th');
  expect(document.documentElement.lang).toBe('th');
});

test('theme_toggle_sets_data_theme_and_persists', async () => {
  document.documentElement.dataset.theme = 'light';
  window.history.replaceState(null, '', '/login');
  stubFetch([401, { code: 'unauthenticated' }]);
  const user = userEvent.setup({ delay: null });
  render(<App />);
  const toggle = await screen.findByRole('button', { name: en['nav.dark_mode'] });
  expect(toggle).toHaveAttribute('aria-pressed', 'false');
  await user.click(toggle);
  expect(document.documentElement.dataset.theme).toBe('dark');
  expect(toggle).toHaveAttribute('aria-pressed', 'true');
  expect(localStorage.getItem('kw-theme')).toBe('dark');
});
