import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { setLocale } from '../i18n';
import en from '../i18n/en.json';
import th from '../i18n/th.json';
import { sentJson, stubFetch } from '../testing/fetch';
import { InviteAccept } from './InviteAccept';

const catalogues = { en, th };

beforeEach(() => window.history.replaceState(null, '', '/'));

afterEach(() => {
  vi.unstubAllGlobals();
  act(() => setLocale('en'));
});

async function choose(messages: Record<string, string>, password: string, again = password) {
  const user = userEvent.setup({ delay: null });
  await user.type(screen.getByLabelText(messages['invite.password']!), password);
  await user.type(screen.getByLabelText(messages['invite.again']!), again);
  await user.click(screen.getByRole('button', { name: messages['invite.submit'] }));
}

test('invite_accept_sets_a_password_and_hides_the_token', async () => {
  window.history.replaceState(null, '', '/invite?token=token-from-link');
  const fetchMock = stubFetch([204]);
  const onAccepted = vi.fn();
  render(<InviteAccept onAccepted={onAccepted} />);

  expect(await screen.findByRole('heading', { level: 1, name: en['invite.title'] })).toBeVisible();
  expect(window.location.pathname).toBe('/invite');
  expect(window.location.search).toBe('');
  expect(window.location.href).not.toContain('token-from-link');
  // The token lives only in the page's memory, never in the history entry.
  expect(window.history.state).toBeNull();

  await choose(en, 'a new long password');
  expect(await screen.findByRole('status')).toHaveTextContent(en['invite.done']);
  expect(onAccepted).toHaveBeenCalledOnce();
  expect(screen.getByRole('link', { name: en['reset.back_to_login'] })).toHaveAttribute(
    'href',
    '/login',
  );
  expect(fetchMock).toHaveBeenCalledOnce();
  expect(fetchMock.mock.calls[0]![0]).toBe('/api/auth/invite/accept');
  expect(fetchMock.mock.calls[0]![1]?.method).toBe('POST');
  expect(sentJson(fetchMock)).toEqual({
    token: 'token-from-link',
    new_password: 'a new long password',
  });
  expect(window.location.href).not.toContain('token-from-link');
});

test('invite_token_is_not_kept_for_a_reload', async () => {
  window.history.replaceState(null, '', '/invite?token=token-from-link');
  const fetchMock = stubFetch([204]);
  const first = render(<InviteAccept />);
  await screen.findByRole('heading', { level: 1, name: en['invite.title'] });
  expect(window.location.search).toBe('');
  expect(window.history.state).toBeNull();
  first.unmount();

  // A reload starts again from this history entry, and nothing in it holds the token.
  render(<InviteAccept />);
  expect(await screen.findByRole('alert')).toHaveTextContent(en['invite.no_token']);
  expect(screen.queryByRole('button', { name: en['invite.submit'] })).toBeNull();
  expect(fetchMock).not.toHaveBeenCalled();
});

// A full-document navigation can keep the whole page, token included, in the back-forward
// cache, and restore it without a popstate. Leaving or coming back that way drops the token.
test.each([
  ['the_page_is_hidden', 'pagehide', false],
  ['the_page_returns_from_the_back_forward_cache', 'pageshow', true],
] as const)('invite_token_is_dropped_when_%s', async (_, type, persisted) => {
  window.history.replaceState(null, '', '/invite?token=token-from-link');
  const fetchMock = stubFetch([204]);
  render(<InviteAccept />);
  expect(await screen.findByLabelText(en['invite.password'])).toBeVisible();
  act(() => {
    window.dispatchEvent(new PageTransitionEvent(type, { persisted }));
  });
  expect(await screen.findByRole('alert')).toHaveTextContent(en['invite.no_token']);
  expect(screen.queryByLabelText(en['invite.password'])).toBeNull();
  expect(fetchMock).not.toHaveBeenCalled();
});

test('a_first_pageshow_keeps_the_invite_token', async () => {
  window.history.replaceState(null, '', '/invite?token=token-from-link');
  stubFetch([204]);
  render(<InviteAccept />);
  await screen.findByLabelText(en['invite.password']);
  act(() => {
    window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: false }));
  });
  expect(screen.getByLabelText(en['invite.password'])).toBeVisible();
  expect(screen.queryByRole('alert')).toBeNull();
});

test.each(['en', 'th'] as const)(
  'invite_accept_explains_an_invalid_or_used_link (%s)',
  async (locale) => {
    const messages: Record<string, string> = catalogues[locale];
    act(() => setLocale(locale));
    window.history.replaceState(null, '', '/invite?token=used-token');
    stubFetch([400, { code: 'invite_token_invalid' }]);
    const onAccepted = vi.fn();
    render(<InviteAccept onAccepted={onAccepted} />);
    await choose(messages, 'a new long password');

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(messages['reason.invite_token_invalid']!);
    expect(onAccepted).not.toHaveBeenCalled();
    expect(screen.getByRole('link', { name: messages['invite.use_reset'] })).toHaveAttribute(
      'href',
      '/reset',
    );
    expect(window.location.href).not.toContain('used-token');
  },
);

test('invite_without_token_explains_the_link_is_incomplete', async () => {
  window.history.replaceState(null, '', '/invite');
  const fetchMock = stubFetch([204]);
  render(<InviteAccept />);
  expect(await screen.findByRole('alert')).toHaveTextContent(en['invite.no_token']);
  expect(screen.queryByRole('button', { name: en['invite.submit'] })).toBeNull();
  expect(screen.getByRole('link', { name: en['invite.use_reset'] })).toHaveAttribute(
    'href',
    '/reset',
  );
  expect(fetchMock).not.toHaveBeenCalled();
});

test('invite_accept_checks_the_password_before_sending', async () => {
  window.history.replaceState(null, '', '/invite?token=t');
  const fetchMock = stubFetch([204]);
  render(<InviteAccept />);
  await choose(en, 'short');
  expect(await screen.findByRole('alert')).toHaveTextContent(en['reason.password_invalid']);
  await choose(en, 'xa new long password', 'a different password');
  expect(await screen.findByRole('alert')).toHaveTextContent(en['invite.mismatch']);
  expect(fetchMock).not.toHaveBeenCalled();
});
