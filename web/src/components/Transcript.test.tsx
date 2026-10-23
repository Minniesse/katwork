import { act, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import { setLocale } from '../i18n';
import en from '../i18n/en.json';
import th from '../i18n/th.json';
import { emptyTranscript, foldEvent, transcriptMessages } from '../sse';
import { ev, numbered } from '../testing/agui';
import { Transcript } from './Transcript';

afterEach(() => {
  act(() => setLocale('en'));
  vi.useRealTimers();
});

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

describe('new-arrival announcer', () => {
  // Ids sort in time order, as UUIDv7 ids do.
  const id = (n: number) => `m-${String(n).padStart(4, '0')}`;
  const range = (from: number, to: number) =>
    Array.from({ length: to - from + 1 }, (_, i) => from + i);
  const stored = (ns: number[], body = (n: number) => `message ${n}`) =>
    messagesFrom(ns.map((n) => ev.stored(id(n), body(n))));
  // By role alone: its name changes with the locale.
  const log = () => screen.getByRole('log');
  const lines = () => [...log().children].map((node) => node.textContent);

  test('announcer_is_visually_hidden_but_exposed_to_assistive_tech', () => {
    render(<Transcript messages={stored([1])} />);
    expect(screen.getByRole('log', { name: en['chat.new_messages'] })).toBe(log());
    // getByRole only finds what the accessibility tree holds; sr-only hides it from sight only.
    expect(log()).toHaveClass('sr-only');
    expect(log()).not.toHaveAttribute('aria-hidden');
    expect(log().closest('[aria-hidden="true"]')).toBeNull();
  });

  test('a_late_reply_after_twenty_newer_arrivals_is_announced', () => {
    vi.useFakeTimers();
    const shown = range(51, 100);
    const { rerender } = render(<Transcript messages={stored(shown)} />);
    for (const n of range(101, 120)) {
      shown.push(n);
      rerender(<Transcript messages={stored(shown)} />);
      // Each arrival comes after a quiet second, so each is its own line, and the log stays
      // small however many arrive.
      expect(log().children.length).toBeLessThanOrEqual(5);
      act(() => vi.advanceTimersByTime(1_100));
    }
    // The reply's id was minted at its run's start, before every message above.
    shown.push(25);
    rerender(
      <Transcript
        messages={stored(
          shown.sort((a, b) => a - b),
          (n) => (n === 25 ? 'Late reply' : `message ${n}`),
        )}
      />,
    );
    expect(lines()).toContain('Kat: Late reply');
  });

  test('a_burst_of_arrivals_is_one_summary_line', () => {
    vi.useFakeTimers();
    const { rerender } = render(<Transcript messages={stored(range(1, 10))} />);
    // A catch-up: twelve stored messages land at once.
    rerender(<Transcript messages={stored(range(1, 22))} />);
    expect(lines()).toEqual([]);
    act(() => vi.advanceTimersByTime(1_000));
    expect(lines()).toEqual(['12 new messages']);
    act(() => setLocale('th'));
    expect(lines()).toEqual(['มีข้อความใหม่ 12 ข้อความ']);
  });

  test('arrivals_one_by_one_in_a_rush_say_three_then_sum_up_the_rest', () => {
    vi.useFakeTimers();
    const shown = range(1, 10);
    const { rerender } = render(<Transcript messages={stored(shown)} />);
    for (const n of range(11, 19)) {
      shown.push(n);
      rerender(<Transcript messages={stored(shown)} />);
      act(() => vi.advanceTimersByTime(100));
    }
    act(() => vi.advanceTimersByTime(1_000));
    expect(lines()).toEqual([
      'Kat: message 11',
      'Kat: message 12',
      'Kat: message 13',
      '6 new messages',
    ]);
  });

  test('a_single_new_message_summary_is_singular', () => {
    vi.useFakeTimers();
    const shown = range(1, 3);
    const { rerender } = render(<Transcript messages={stored(shown)} />);
    for (const n of range(4, 7)) {
      shown.push(n);
      rerender(<Transcript messages={stored(shown)} />);
    }
    act(() => vi.advanceTimersByTime(1_000));
    expect(lines().at(-1)).toBe('1 new message');
  });

  test('the_log_never_holds_more_than_five_lines', () => {
    vi.useFakeTimers();
    const shown = [1];
    const { rerender } = render(<Transcript messages={stored(shown)} />);
    // Two rushes of three, a quiet second apart: six single lines inside one clearing period.
    for (const rushOf of [range(2, 4), range(5, 7)]) {
      for (const n of rushOf) {
        shown.push(n);
        rerender(<Transcript messages={stored(shown)} />);
      }
      act(() => vi.advanceTimersByTime(1_100));
    }
    expect(lines()).toEqual([3, 4, 5, 6, 7].map((n) => `Kat: message ${n}`));
  });

  test('unmounting_with_a_line_waiting_to_clear_leaves_no_timer', () => {
    vi.useFakeTimers();
    const { rerender, unmount } = render(<Transcript messages={stored([1])} />);
    rerender(<Transcript messages={stored([1, 2])} />);
    expect(lines()).toEqual(['Kat: message 2']);
    unmount();
    expect(vi.getTimerCount()).toBe(0);
    act(() => vi.advanceTimersByTime(10_000));
    expect(vi.getTimerCount()).toBe(0);
  });

  test('unmounting_with_a_burst_summary_pending_leaves_no_timer', () => {
    vi.useFakeTimers();
    const { rerender, unmount } = render(<Transcript messages={stored([1])} />);
    rerender(<Transcript messages={stored(range(1, 6))} />);
    // Five at once: the summary waits for a quiet second.
    expect(lines()).toEqual([]);
    unmount();
    expect(vi.getTimerCount()).toBe(0);
    act(() => vi.advanceTimersByTime(10_000));
    expect(vi.getTimerCount()).toBe(0);
  });

  test('unmounting_with_a_summary_line_shown_leaves_no_timer', () => {
    vi.useFakeTimers();
    const { rerender, unmount } = render(<Transcript messages={stored([1])} />);
    rerender(<Transcript messages={stored(range(1, 6))} />);
    act(() => vi.advanceTimersByTime(1_000));
    expect(lines()).toEqual(['5 new messages']);
    unmount();
    expect(vi.getTimerCount()).toBe(0);
    act(() => vi.advanceTimersByTime(10_000));
    expect(vi.getTimerCount()).toBe(0);
  });

  test('announcements_are_cleared_after_they_are_read', () => {
    vi.useFakeTimers();
    const { rerender } = render(<Transcript messages={stored([1])} />);
    rerender(<Transcript messages={stored([1, 2])} />);
    expect(lines()).toEqual(['Kat: message 2']);
    act(() => vi.advanceTimersByTime(4_000));
    expect(lines()).toEqual(['Kat: message 2']);
    act(() => vi.advanceTimersByTime(1_000));
    // A browsing screen reader doesn't meet the message twice.
    expect(log()).toBeEmptyDOMElement();
  });

  test('history_and_snapshot_are_never_announced_even_after_earlier_arrivals', () => {
    vi.useFakeTimers();
    const snapshot = new Set(range(101, 150).map(id));
    const shown = range(101, 150);
    const { rerender } = render(<Transcript messages={stored(shown)} history={snapshot} />);
    expect(log()).toBeEmptyDOMElement();
    // Delayed replies with old ids arrive live: 1-20, then 51-70.
    for (const batch of [range(1, 20), range(51, 70)]) {
      shown.push(...batch);
      rerender(
        <Transcript messages={stored([...shown].sort((a, b) => a - b))} history={snapshot} />,
      );
      act(() => vi.advanceTimersByTime(10_000));
    }
    expect(log()).toBeEmptyDOMElement();

    // An older page (51-100) overlaps the live 51-70; nothing is announced for it.
    const added: Node[] = [];
    const observer = new MutationObserver((records) =>
      records.forEach((record) => added.push(...record.addedNodes)),
    );
    observer.observe(log(), { childList: true });
    shown.push(...range(71, 100));
    const history = new Set([...snapshot, ...range(51, 100).map(id)]);
    rerender(<Transcript messages={stored([...shown].sort((a, b) => a - b))} history={history} />);
    // Collected synchronously: a MutationObserver callback runs only after the test's own code,
    // so records are taken before each step can hide them, and before disconnecting.
    const take = () => observer.takeRecords().forEach((record) => added.push(...record.addedNodes));
    take();
    act(() => vi.advanceTimersByTime(1_000)); // the burst summary's deadline
    take();
    act(() => vi.advanceTimersByTime(5_000)); // the lines' clearing
    take();
    observer.disconnect();
    expect(added).toHaveLength(0);
  });
});

test('missing_mention_space_names_the_kat_once_in_both_locales', () => {
  const messages = messagesFrom([
    {
      type: 'CUSTOM',
      name: 'katwork.message_stored',
      value: {
        messageId: 'notice',
        author: { kind: 'system' },
        body: 'mention_needs_space',
        meta: { code: 'mention_needs_space', kats: [{ id: 'writer', name: 'เลขา' }] },
      },
    },
  ]);
  render(<Transcript messages={messages} />);
  const notice = screen.getByText('Add a space after @เลขา to mention this Kat.');
  expect(notice).toBeVisible();
  // The sentence names the Kat, so no "About:" line repeats it.
  const item = notice.closest('li')!;
  expect(within(item).getAllByText(/เลขา/)).toHaveLength(1);
  act(() => setLocale('th'));
  expect(screen.getByText('โปรดเว้นวรรคหลัง @เลขา เพื่อเรียก Kat คนนี้')).toBeVisible();
  expect(within(item).getAllByText(/เลขา/)).toHaveLength(1);
});
