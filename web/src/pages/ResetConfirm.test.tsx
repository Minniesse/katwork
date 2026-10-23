import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, test, vi } from 'vitest';
import en from '../i18n/en.json';
import { sentJson, stubFetch } from '../testing/fetch';
import { ResetConfirm } from './ResetConfirm';

afterEach(() => vi.unstubAllGlobals());

async function choose(password: string, again = password) {
  const user = userEvent.setup({ delay: null });
  await user.type(screen.getByLabelText(en['reset.confirm.password']), password);
  await user.type(screen.getByLabelText(en['reset.confirm.again']), again);
  await user.click(screen.getByRole('button', { name: en['reset.confirm.submit'] }));
}

test('reset_confirm_sends_token_and_new_password', async () => {
  const fetchMock = stubFetch([200, {}]);
  const onReset = vi.fn();
  render(<ResetConfirm token="token-from-link" onReset={onReset} />);
  await choose('a new long password');

  expect(await screen.findByRole('status')).toHaveTextContent(en['reset.confirm.done']);
  expect(onReset).toHaveBeenCalledOnce();
  expect(screen.getByRole('link', { name: en['reset.back_to_login'] })).toHaveAttribute(
    'href',
    '/login',
  );
  expect(fetchMock.mock.calls[0]![0]).toBe('/api/auth/reset/confirm');
  expect(sentJson(fetchMock)).toEqual({
    token: 'token-from-link',
    new_password: 'a new long password',
  });
});

test('reset_confirm_explains_an_invalid_or_used_link', async () => {
  stubFetch([400, { code: 'reset_token_invalid' }]);
  const onReset = vi.fn();
  render(<ResetConfirm token="used-token" onReset={onReset} />);
  await choose('a new long password');
  expect(await screen.findByRole('alert')).toHaveTextContent(en['reason.reset_token_invalid']);
  expect(onReset).not.toHaveBeenCalled();
  expect(screen.getByRole('link', { name: en['reset.request_again'] })).toHaveAttribute(
    'href',
    '/reset',
  );
});

test('reset_confirm_checks_length_before_sending', async () => {
  const fetchMock = stubFetch([200, {}]);
  render(<ResetConfirm token="t" onReset={() => {}} />);
  await choose('short');
  expect(await screen.findByRole('alert')).toHaveTextContent(en['reason.password_invalid']);
  expect(fetchMock).not.toHaveBeenCalled();
});

test('reset_confirm_rejects_mismatched_passwords', async () => {
  const fetchMock = stubFetch([200, {}]);
  render(<ResetConfirm token="t" onReset={() => {}} />);
  await choose('a new long password', 'a different password');
  expect(await screen.findByRole('alert')).toHaveTextContent(en['reset.confirm.mismatch']);
  expect(fetchMock).not.toHaveBeenCalled();
});
