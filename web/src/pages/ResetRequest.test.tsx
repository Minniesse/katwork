import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, test, vi } from 'vitest';
import en from '../i18n/en.json';
import { sentJson, stubFetch } from '../testing/fetch';
import { ResetRequest } from './ResetRequest';

afterEach(() => vi.unstubAllGlobals());

async function request(email: string) {
  const user = userEvent.setup({ delay: null });
  const view = render(<ResetRequest />);
  await user.type(screen.getByLabelText(en['reset.email']), email);
  await user.click(screen.getByRole('button', { name: en['reset.request.submit'] }));
  const status = await screen.findByRole('status');
  const text = status.textContent;
  view.unmount();
  return text;
}

test('reset_request_shows_same_message_regardless_of_email', async () => {
  const fetchMock = stubFetch([202, {}]);
  const known = await request('person@example.test');
  const unknown = await request('nobody@example.test');

  expect(known).toBe(unknown);
  expect(known).toContain(en['reset.request.sent']);
  expect(known).not.toContain('person@example.test');
  expect(sentJson(fetchMock, 0)).toEqual({ email: 'person@example.test' });
  expect(sentJson(fetchMock, 1)).toEqual({ email: 'nobody@example.test' });
});

test('reset_request_network_failure_is_shown', async () => {
  vi.stubGlobal('fetch', vi.fn(async () => Promise.reject(new TypeError('offline'))));
  const user = userEvent.setup({ delay: null });
  render(<ResetRequest />);
  await user.type(screen.getByLabelText(en['reset.email']), 'person@example.test');
  await user.click(screen.getByRole('button', { name: en['reset.request.submit'] }));
  expect(await screen.findByRole('alert')).toHaveTextContent(en['reason.network_error']);
  expect(screen.queryByRole('status')).toBeNull();
});
