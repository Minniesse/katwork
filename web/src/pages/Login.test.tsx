import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, test, vi } from 'vitest';
import { setLocale, type Locale } from '../i18n';
import en from '../i18n/en.json';
import th from '../i18n/th.json';
import { sentJson, stubFetch } from '../testing/fetch';
import { Login } from './Login';

const catalogues = { en, th };

afterEach(() => {
  vi.unstubAllGlobals();
  act(() => setLocale('en'));
});

async function signIn(copy: typeof en, email: string, password: string) {
  const user = userEvent.setup({ delay: null });
  await user.type(screen.getByLabelText(copy['login.email']), email);
  await user.type(screen.getByLabelText(copy['login.password']), password);
  await user.click(screen.getByRole('button', { name: copy['login.submit'] }));
}

test.each<Locale>(['en', 'th'])('login_shows_localized_error_on_401 (%s)', async (locale) => {
  act(() => setLocale(locale));
  const copy = catalogues[locale];
  stubFetch([401, { code: 'unauthenticated' }]);
  const onSignedIn = vi.fn();
  render(<Login onSignedIn={onSignedIn} />);

  await signIn(copy, 'person@example.test', 'wrong password');

  const alert = await screen.findByRole('alert');
  expect(alert).toHaveTextContent(copy['reason.unauthenticated']);
  expect(onSignedIn).not.toHaveBeenCalled();
  expect(screen.getByLabelText(copy['login.password'])).toHaveAttribute('aria-invalid', 'true');
});

test('login_success_reports_the_user', async () => {
  const fetchMock = stubFetch([200, { user_id: 'user-1' }]);
  const onSignedIn = vi.fn();
  render(<Login onSignedIn={onSignedIn} />);

  await signIn(en, 'person@example.test', 'correct horse');

  await vi.waitFor(() => expect(onSignedIn).toHaveBeenCalledWith({ user_id: 'user-1' }));
  expect(sentJson(fetchMock)).toEqual({ email: 'person@example.test', password: 'correct horse' });
  expect(screen.queryByRole('alert')).toBeNull();
});

test('login_rate_limit_is_explained', async () => {
  stubFetch([429, { code: 'rate_limited' }]);
  render(<Login onSignedIn={vi.fn()} />);
  await signIn(en, 'person@example.test', 'again');
  expect(await screen.findByRole('alert')).toHaveTextContent(en['reason.rate_limited']);
});

test('login_fields_have_autocomplete_hints_and_a_reset_link', () => {
  render(<Login onSignedIn={vi.fn()} />);
  expect(screen.getByLabelText(en['login.email'])).toHaveAttribute('autocomplete', 'username');
  expect(screen.getByLabelText(en['login.password'])).toHaveAttribute(
    'autocomplete',
    'current-password',
  );
  expect(screen.getByRole('link', { name: en['login.forgot'] })).toHaveAttribute('href', '/reset');
});
