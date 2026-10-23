import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { App } from './app';
import { setLocale } from './i18n';
import en from './i18n/en.json';
import th from './i18n/th.json';
import { navigate } from './router';
import { stubFetch } from './testing/fetch';

beforeEach(() => window.history.replaceState(null, '', '/'));

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

test('signed_in_home_shows_team_and_chat_placeholders', async () => {
  stubFetch([200, { user_id: 'user-1' }]);
  render(<App />);
  expect(await screen.findByRole('heading', { level: 1, name: en['home.title'] })).toBeVisible();
  expect(screen.getByRole('region', { name: en['home.team.title'] })).toBeInTheDocument();
  expect(screen.getByRole('region', { name: en['home.chat.title'] })).toBeInTheDocument();
});

test('login_then_home', async () => {
  window.history.replaceState(null, '', '/login');
  stubFetch([401, { code: 'unauthenticated' }], [200, { user_id: 'user-1' }]);
  const user = userEvent.setup({ delay: null });
  render(<App />);
  await user.type(await screen.findByLabelText(en['login.email']), 'person@example.test');
  await user.type(screen.getByLabelText(en['login.password']), 'correct horse');
  await user.click(screen.getByRole('button', { name: en['login.submit'] }));
  expect(await screen.findByRole('heading', { level: 1, name: en['home.title'] })).toBeVisible();
  expect(window.location.pathname).toBe('/');
});

test('sign_out_returns_to_login', async () => {
  const fetchMock = stubFetch([200, { user_id: 'user-1' }], [200, {}]);
  const user = userEvent.setup({ delay: null });
  render(<App />);
  await user.click(await screen.findByRole('button', { name: en['nav.sign_out'] }));
  expect(await screen.findByRole('heading', { level: 1, name: en['login.title'] })).toBeVisible();
  expect(fetchMock.mock.calls[1]![0]).toBe('/api/auth/logout');
  expect(fetchMock.mock.calls[1]![1]?.method).toBe('POST');
});

test('failed_sign_out_keeps_the_session_and_can_be_retried', async () => {
  const fetchMock = stubFetch(
    [200, { user_id: 'user-1' }],
    [503, { code: 'database_unavailable' }],
    [200, {}],
  );
  const user = userEvent.setup({ delay: null });
  render(<App />);
  await user.click(await screen.findByRole('button', { name: en['nav.sign_out'] }));

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
  expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
    '/api/auth/me',
    '/api/auth/logout',
    '/api/auth/logout',
  ]);
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
