import { act, render, screen, within } from '@testing-library/react';
import { afterEach, expect, test } from 'vitest';
import { setLocale } from '../i18n';
import en from '../i18n/en.json';
import th from '../i18n/th.json';
import { emptyTranscript, foldEvent, transcriptMessages } from '../sse';
import { ev, numbered } from '../testing/agui';
import { Transcript } from './Transcript';

afterEach(() => act(() => setLocale('en')));

const messagesFrom = (events: Parameters<typeof numbered>[0]) =>
  transcriptMessages(numbered(events).reduce(foldEvent, emptyTranscript()));

test('transcript_is_live_region', () => {
  const streaming = messagesFrom([
    ev.userStored('u1', 'Hello'),
    ev.runStarted('r1'),
    ev.kwRunStarted('r1', 'dot-a', 'm1'),
    ev.textStart('m1'),
    ev.text('m1', 'Hi'),
  ]);
  const { rerender } = render(<Transcript messages={streaming} />);
  const log = screen.getByRole('log', { name: en['chat.transcript'] });
  expect(log).toHaveAttribute('aria-live', 'polite');
  expect(log).toHaveAttribute('aria-relevant', 'additions');
  // While a reply streams, assistive tech waits for the finished text.
  expect(log).toHaveAttribute('aria-busy', 'true');
  expect(within(log).getAllByRole('listitem')).toHaveLength(2);

  const done = messagesFrom([ev.userStored('u1', 'Hello'), ev.stored('m1', 'Hi there')]);
  rerender(<Transcript messages={done} />);
  expect(log).toHaveAttribute('aria-busy', 'false');
  expect(within(log).getByText('Hi there')).toBeInTheDocument();
});

test('synthetic_reply_shows_badge', () => {
  const synthetic = messagesFrom([ev.stored('m1', 'Here is a sample reply.', { synthetic: true })]);
  const { rerender } = render(<Transcript messages={synthetic} />);
  const item = screen.getByText('Here is a sample reply.').closest('li')!;
  expect(within(item).getByText(en['chat.synthetic_badge'])).toBeVisible();

  act(() => setLocale('th'));
  rerender(<Transcript messages={synthetic} />);
  expect(within(item).getByText(th['chat.synthetic_badge'])).toBeVisible();

  const real = messagesFrom([ev.stored('m1', 'Here is a sample reply.', { synthetic: false })]);
  rerender(<Transcript messages={real} />);
  expect(within(screen.getByText('Here is a sample reply.').closest('li')!).queryByText(
    th['chat.synthetic_badge'],
  )).toBeNull();
});

test('system_notice_shows_localized_reason', () => {
  const messages = messagesFrom([ev.notice('n1', 'provider_unavailable')]);
  render(<Transcript messages={messages} />);
  expect(screen.getByText(en['reason.provider_unavailable'])).toBeInTheDocument();
  expect(screen.queryByText('provider_unavailable')).toBeNull();
});

test('dot_messages_use_the_given_dot_name', () => {
  const messages = messagesFrom([ev.stored('m1', 'Done.', { author: { kind: 'dot', id: 'dot-a' } })]);
  render(<Transcript messages={messages} dotName={(id) => (id === 'dot-a' ? 'Mali' : undefined)} />);
  expect(screen.getByText('Mali')).toBeInTheDocument();
});

test('empty_transcript_invites_a_first_message', () => {
  render(<Transcript messages={[]} />);
  expect(screen.getByText(en['chat.empty'])).toBeInTheDocument();
});
