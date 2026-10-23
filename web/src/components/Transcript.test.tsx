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
  const first = messagesFrom([ev.userStored('u1', 'Hello')]);
  const { rerender } = render(<Transcript messages={first} />);
  // The conversation is a list in message-id order; the log announces new arrivals only.
  const list = screen.getByRole('list', { name: en['chat.transcript'] });
  const log = screen.getByRole('log', { name: en['chat.new_messages'] });
  expect(log).toHaveAttribute('aria-live', 'polite');
  expect(log).toHaveAttribute('aria-relevant', 'additions');
  expect(list.closest('[aria-live]')).toBeNull();
  // What was there when the page loaded is not announced.
  expect(log).toBeEmptyDOMElement();

  const streaming = messagesFrom([
    ev.userStored('u1', 'Hello'),
    ev.runStarted('r1'),
    ev.kwRunStarted('r1', 'kat-a', 'u2'),
    ev.textStart('u2'),
    ev.text('u2', 'Hi'),
  ]);
  rerender(<Transcript messages={streaming} />);
  // Streaming text is hidden from assistive tech and never announced.
  expect(within(list).getByText('Hi').closest('li')).toHaveAttribute('aria-hidden', 'true');
  expect(log).toBeEmptyDOMElement();

  const done = messagesFrom([ev.userStored('u1', 'Hello'), ev.stored('u2', 'Hi there')]);
  rerender(<Transcript messages={done} />);
  expect([...log.children].map((node) => node.textContent)).toEqual(['Kat: Hi there']);
  expect(within(list).getAllByRole('listitem')).toHaveLength(2);
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
  expect(
    within(screen.getByText('Here is a sample reply.').closest('li')!).queryByText(
      th['chat.synthetic_badge'],
    ),
  ).toBeNull();
});

test('system_notice_shows_localized_reason', () => {
  const messages = messagesFrom([ev.notice('n1', 'provider_unavailable')]);
  render(<Transcript messages={messages} />);
  expect(screen.getByText(en['reason.provider_unavailable'])).toBeInTheDocument();
  expect(screen.queryByText('provider_unavailable')).toBeNull();
});

test('pause_notices_distinguish_stopped_work_from_queued_work_in_both_locales', () => {
  const messages = messagesFrom([
    ev.notice('queued', 'workspace_paused'),
    ev.notice('workspace-stopped', 'workspace_paused_stopped'),
    ev.notice('kat-stopped', 'kat_paused_stopped'),
    ev.notice('approval', 'approval_pending'),
  ]);
  render(<Transcript messages={messages} />);
  expect(
    screen.getByText(
      "Your admin has paused the Kats in this workspace. The work waits until they're resumed.",
    ),
  ).toBeVisible();
  expect(
    screen.getByText(
      'Stopped because the Kats in this workspace were paused. Send the request again after they resume.',
    ),
  ).toBeVisible();
  expect(
    screen.getByText(
      'Stopped because this Kat was paused. Send the request again after it resumes, or ask another Kat.',
    ),
  ).toBeVisible();
  expect(
    screen.getByText(
      'This action needs approval. Approving actions is not available yet, so this request has ended.',
    ),
  ).toBeVisible();

  act(() => setLocale('th'));
  expect(
    screen.getByText(
      'ผู้ดูแลระบบหยุด Kats ในเวิร์กสเปซนี้ไว้ชั่วคราว งานจะรอจนกว่าจะเปิดใช้อีกครั้ง',
    ),
  ).toBeVisible();
  expect(
    screen.getByText(
      'งานหยุดแล้ว เพราะ Kats ในเวิร์กสเปซนี้ถูกหยุดไว้ชั่วคราว โปรดส่งคำขออีกครั้งเมื่อเปิดใช้แล้ว',
    ),
  ).toBeVisible();
  expect(
    screen.getByText(
      'งานหยุดแล้ว เพราะ Kat ที่รับงานนี้ถูกหยุดไว้ชั่วคราว โปรดส่งคำขออีกครั้งเมื่อเปิดใช้แล้ว หรือขอให้ Kat อื่นช่วยแทน',
    ),
  ).toBeVisible();
  expect(
    screen.getByText('ขั้นตอนนี้ต้องได้รับอนุมัติ แต่ยังไม่มีระบบอนุมัติ คำขอนี้จึงจบแล้ว'),
  ).toBeVisible();
});

test('kat_messages_use_the_given_kat_name', () => {
  const messages = messagesFrom([
    ev.stored('m1', 'Done.', { author: { kind: 'kat', id: 'kat-a' } }),
  ]);
  render(
    <Transcript messages={messages} katName={(id) => (id === 'kat-a' ? 'Mali' : undefined)} />,
  );
  expect(screen.getByText('Mali')).toBeInTheDocument();
});

test('empty_transcript_invites_a_first_message', () => {
  render(<Transcript messages={[]} />);
  expect(screen.getByText(en['chat.empty'])).toBeInTheDocument();
});

test('stopped_reply_is_labelled_in_both_locales', () => {
  const messages = messagesFrom([ev.stored('m1', 'Hello', { stopped: true })]);
  render(<Transcript messages={messages} />);
  const item = screen.getByText('Hello').closest('li')!;
  expect(within(item).getByText(en['chat.stopped_label'])).toBeVisible();
  act(() => setLocale('th'));
  expect(within(item).getByText(th['chat.stopped_label'])).toBeVisible();
});

test('people_are_named_and_the_signed_in_person_is_you', () => {
  const messages = messagesFrom([
    ev.userStored('m1', 'Mine', { userId: 'user-1', authorName: 'Pat' }),
    ev.userStored('m2', 'Theirs', { userId: 'user-2', authorName: 'สมชาย' }),
  ]);
  render(<Transcript messages={messages} me="user-1" />);
  expect(
    within(screen.getByText('Mine').closest('li')!).getByText(en['chat.author.you']),
  ).toBeVisible();
  expect(within(screen.getByText('Theirs').closest('li')!).getByText('สมชาย')).toBeVisible();
});

test('notice_names_the_kats_in_its_meta', () => {
  const messages = messagesFrom([
    ev.notice('n1', 'kat_paused', [
      { id: 'kat-w', name: 'นักเขียน' },
      { id: 'kat-a', name: 'Analyst' },
    ]),
  ]);
  render(<Transcript messages={messages} />);
  expect(screen.getByText(en['reason.kat_paused'])).toBeVisible();
  expect(screen.getByText('About: นักเขียน, Analyst')).toBeVisible();
});

test('question_options_are_buttons_only_for_the_active_question', () => {
  const question = { question: 'Which format?', options: ['PDF', 'Word'] };
  const messages = messagesFrom([
    ev.stored('m1', 'Which format?', { question }),
    ev.stored('m2', 'Which format?', { question }),
  ]);
  render(<Transcript messages={messages} activeQuestion="m2" onOption={() => {}} />);
  const [first, second] = screen.getAllByText('Which format?').map((node) => node.closest('li')!);
  expect(within(first!).queryAllByRole('button')).toHaveLength(0);
  expect(within(first!).getByText('PDF')).toBeVisible();
  const group = within(second!).getByRole('group', { name: en['chat.question_options'] });
  expect(
    within(group)
      .getAllByRole('button')
      .map((button) => button.textContent),
  ).toEqual(['PDF', 'Word']);
});
