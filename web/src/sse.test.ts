import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import {
  emptyTranscript,
  foldEvent,
  parseAuthor,
  subscribe,
  transcriptFromSnapshot,
  transcriptMessages,
  type AgUiEvent,
  type SeqEvent,
  type Transcript,
} from './sse';
import { ev, numbered } from './testing/agui';

class MockEventSource {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSED = 2;
  static instances: MockEventSource[] = [];

  readyState = MockEventSource.CONNECTING;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;

  constructor(
    readonly url: string,
    readonly init?: EventSourceInit,
  ) {
    MockEventSource.instances.push(this);
  }

  close() {
    this.readyState = MockEventSource.CLOSED;
  }

  emit({ seq, event }: SeqEvent) {
    this.readyState = MockEventSource.OPEN;
    this.onmessage?.(
      new MessageEvent('message', { data: JSON.stringify(event), lastEventId: String(seq) }),
    );
  }

  emitRaw(data: string, lastEventId: string) {
    this.onmessage?.(new MessageEvent('message', { data, lastEventId }));
  }

  /** The connection drops; the browser reconnects by itself and sends Last-Event-ID. */
  drop() {
    this.readyState = MockEventSource.CONNECTING;
    this.onerror?.(new Event('error'));
  }

  /** The browser gave up (for example a non-200 response); nothing reconnects by itself. */
  fail() {
    this.readyState = MockEventSource.CLOSED;
    this.onerror?.(new Event('error'));
  }
}

beforeEach(() => {
  MockEventSource.instances = [];
  vi.stubGlobal('EventSource', MockEventSource);
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

const fold = (events: SeqEvent[], start: Transcript = emptyTranscript()) =>
  events.reduce(foldEvent, start);

const shown = (state: Transcript) =>
  transcriptMessages(state).map((m) => ({ id: m.id, body: m.body, provisional: m.provisional }));

const stored = (...pairs: [string, string][]) =>
  pairs.map(([id, body]) => ({ id, body, provisional: false }));

/** Folds the log once straight through, and once through a reconnect that replays from `from`. */
function foldWithReconnect(events: AgUiEvent[], cut: number, from: number) {
  const log = numbered(events);
  const straight = fold(log);
  const resumed = fold(log.slice(from - 1), fold(log.slice(0, cut)));
  return { straight, resumed };
}

describe('subscribe', () => {
  test('sse_folds_by_id_and_drops_duplicates_after_reconnect', () => {
    const received: SeqEvent[] = [];
    const stop = subscribe('/api/workspaces/w/threads/t/events', (e) => received.push(e));
    const [source] = MockEventSource.instances;
    expect(source?.url).toBe('/api/workspaces/w/threads/t/events');

    const log = numbered([
      ev.userStored('u1', 'Hi team'),
      ev.runStarted('rA'),
      ev.kwRunStarted('rA', 'dot-a', 'mA'),
      ev.runStarted('rB'),
      ev.kwRunStarted('rB', 'dot-b', 'mB'),
      ev.textStart('mA'),
      ev.textStart('mB'),
      ev.text('mA', 'สวัสดี'),
      ev.text('mB', 'Hello'),
      ev.text('mA', 'ครับ 👋🏽'),
      ev.text('mB', ' there'),
    ]);
    log.slice(0, 8).forEach((e) => source!.emit(e));
    source!.drop();
    // The resumed stream overlaps what was already delivered (seq 6 to 8).
    log.slice(5).forEach((e) => source!.emit(e));

    expect(received.map((e) => e.seq)).toEqual(log.map((e) => e.seq));
    expect(MockEventSource.instances).toHaveLength(1);
    expect(shown(fold(received))).toEqual([
      { id: 'u1', body: 'Hi team', provisional: false },
      { id: 'mA', body: 'สวัสดีครับ 👋🏽', provisional: true },
      { id: 'mB', body: 'Hello there', provisional: true },
    ]);

    stop();
    expect(source!.readyState).toBe(MockEventSource.CLOSED);
  });

  test('closed_stream_is_reopened_and_replayed_events_are_skipped', () => {
    vi.useFakeTimers();
    const received: number[] = [];
    subscribe('/events', (e) => received.push(e.seq));
    const log = numbered([ev.userStored('u1', 'one'), ev.userStored('u2', 'two'), ev.userStored('u3', 'three')]);
    MockEventSource.instances[0]!.emit(log[0]!);
    MockEventSource.instances[0]!.emit(log[1]!);
    MockEventSource.instances[0]!.fail();

    expect(MockEventSource.instances).toHaveLength(1);
    vi.advanceTimersByTime(30_000);
    expect(MockEventSource.instances).toHaveLength(2);
    // A new EventSource has no Last-Event-ID, so the server replays from the start.
    log.forEach((e) => MockEventSource.instances[1]!.emit(e));
    expect(received).toEqual([1, 2, 3]);
  });

  test('stop_cancels_a_pending_reopen', () => {
    vi.useFakeTimers();
    const stop = subscribe('/events', () => {});
    MockEventSource.instances[0]!.fail();
    stop();
    vi.advanceTimersByTime(60_000);
    expect(MockEventSource.instances).toHaveLength(1);
  });

  test('malformed_or_unnumbered_events_are_ignored', () => {
    const received: number[] = [];
    subscribe('/events', (e) => received.push(e.seq));
    const source = MockEventSource.instances[0]!;
    source.emitRaw('{not json', '1');
    source.emitRaw(JSON.stringify(ev.userStored('u1', 'hi')), '');
    source.emitRaw(JSON.stringify({ noType: true }), '2');
    source.emit({ seq: 3, event: ev.userStored('u1', 'hi') });
    expect(received).toEqual([3]);
  });
});

describe('foldEvent', () => {
  test('a_duplicate_seq_is_a_no_op', () => {
    const [first] = numbered([ev.userStored('u1', 'hi')]);
    const once = foldEvent(emptyTranscript(), first!);
    expect(foldEvent(once, first!)).toBe(once);
  });

  test('tool_arguments_fold_by_tool_call_id', () => {
    const state = fold(
      numbered([
        ev.runStarted('r1'),
        ev.kwRunStarted('r1', 'dot-a', 'm1'),
        ev.toolStart('tc1', 'search', 'm1'),
        ev.toolStart('tc2', 'read_file', 'm1'),
        ev.toolArgs('tc1', '{"q":'),
        ev.toolArgs('tc2', '{"path":"a.md"}'),
        ev.toolArgs('tc1', '"ภาษีมูลค่าเพิ่ม"}'),
        ev.toolEnd('tc1'),
        ev.toolEnd('tc2'),
        ev.toolResult('m1', 'tc2', 'contents'),
        ev.toolStatus('tc2', true),
      ]),
    );
    expect(state.toolCalls['tc1']).toMatchObject({
      name: 'search',
      args: '{"q":"ภาษีมูลค่าเพิ่ม"}',
      argsComplete: true,
    });
    expect(state.toolCalls['tc2']).toMatchObject({
      name: 'read_file',
      args: '{"path":"a.md"}',
      result: 'contents',
      ok: true,
    });
  });

  test('provisional_text_dropped_after_lease_loss_and_recovery', () => {
    const firstRun = numbered([
      ev.userStored('u1', 'Summarise the report'),
      ev.runStarted('r1'),
      ev.kwRunStarted('r1', 'dot-a', 'm1'),
      ev.dotState('dot-a', 'working'),
      ev.textStart('m1'),
      ev.text('m1', 'Partial text from the lost run'),
      ev.runError('lease_lost'),
      ev.kwRunClosed('r1', 'interrupted'),
    ]);
    const afterLoss = fold(firstRun);
    expect(shown(afterLoss)).toEqual(stored(['u1', 'Summarise the report']));

    const recovery = numbered(
      [
        ev.runStarted('r2'),
        ev.kwRunStarted('r2', 'dot-a', 'm1'),
        ev.dotState('dot-a', 'working'),
        ev.textStart('m1'),
        ev.text('m1', 'Recovered draft'),
        ev.textEnd('m1'),
        ev.decision('j1'),
        ev.stored('m1', 'The stored summary.'),
        ev.jobStatus('j1', 'answered'),
        ev.dotState('dot-a', 'idle'),
        ev.runFinished('r2'),
        ev.kwRunClosed('r2', 'finished'),
      ],
      firstRun.length + 1,
    );
    const final = fold(recovery, afterLoss);
    expect(shown(final)).toEqual(
      stored(['u1', 'Summarise the report'], ['m1', 'The stored summary.']),
    );
  });

  test('interleaved_runs_one_error_one_stored_reply', () => {
    const log = numbered([
      ev.userStored('u1', '@Researcher @Writer please help'),
      ev.runStarted('rA'),
      ev.kwRunStarted('rA', 'dot-a', 'mA'),
      ev.runStarted('rB'),
      ev.kwRunStarted('rB', 'dot-b', 'mB'),
      ev.textStart('mA'),
      ev.textStart('mB'),
      ev.text('mA', 'Researching'),
      ev.text('mB', 'Writing'),
      ev.runError('lease_lost'),
      ev.kwRunClosed('rA', 'interrupted'),
    ]);
    const midway = fold(log);
    // Run B's live text stays while run A is closed.
    expect(shown(midway)).toEqual([
      { id: 'u1', body: '@Researcher @Writer please help', provisional: false },
      { id: 'mB', body: 'Writing', provisional: true },
    ]);

    const rest = numbered(
      [
        ev.text('mB', ' more'),
        ev.textEnd('mB'),
        ev.decision('jB'),
        ev.stored('mB', 'The Writer’s stored reply', { author: { kind: 'dot', id: 'dot-b' } }),
        ev.jobStatus('jB', 'answered'),
        ev.runFinished('rB'),
        ev.kwRunClosed('rB', 'finished'),
      ],
      log.length + 1,
    );
    expect(shown(fold(rest, midway))).toEqual(
      stored(['u1', '@Researcher @Writer please help'], ['mB', 'The Writer’s stored reply']),
    );
  });

  test('provisional_text_dropped_after_provider_failure', () => {
    const state = fold(
      numbered([
        ev.userStored('u1', 'Draft the letter'),
        ev.runStarted('r1'),
        ev.kwRunStarted('r1', 'dot-a', 'm1'),
        ev.textStart('m1'),
        ev.text('m1', 'Dear'),
        ev.decision('j1'),
        ev.notice('n1', 'provider_unavailable'),
        ev.jobStatus('j1', 'stopped', 'provider_unavailable'),
        ev.dotState('dot-a', 'idle'),
        ev.runFinished('r1'),
        ev.kwRunClosed('r1', 'finished'),
      ]),
    );
    expect(shown(state)).toEqual(
      stored(['u1', 'Draft the letter'], ['n1', 'provider_unavailable']),
    );
    expect(transcriptMessages(state)[1]?.author).toEqual({ kind: 'system' });
    expect(state.jobs['j1']).toEqual({ status: 'stopped', reason: 'provider_unavailable', holdReason: null });
  });

  test('final_text_differs_from_deltas_shows_stored_text', () => {
    const state = fold(
      numbered([
        ev.runStarted('r1'),
        ev.kwRunStarted('r1', 'dot-a', 'm1'),
        ev.textStart('m1'),
        ev.text('m1', 'Draft answr'),
        ev.textEnd('m1'),
        ev.stored('m1', 'Final answer.', { synthetic: true }),
        ev.runFinished('r1'),
        ev.kwRunClosed('r1', 'finished'),
      ]),
    );
    expect(transcriptMessages(state)).toEqual([
      expect.objectContaining({
        id: 'm1',
        body: 'Final answer.',
        provisional: false,
        synthetic: true,
        author: { kind: 'dot', id: 'dot-a' },
      }),
    ]);
  });

  test('stored_question_carries_its_options', () => {
    const question = { question: 'Which year?', options: ['2569', '2568'] };
    const state = fold(numbered([ev.stored('m1', 'Which year?', { question })]));
    expect(transcriptMessages(state)[0]?.question).toEqual(question);
  });

  test('new_turn_start_resets_provisional_text', () => {
    const state = fold(
      numbered([
        ev.runStarted('r1'),
        ev.kwRunStarted('r1', 'dot-a', 'm1'),
        ev.textStart('m1'),
        ev.text('m1', 'Let me look that up.'),
        ev.textEnd('m1'),
        ev.toolStart('tc1', 'search', 'm1'),
        ev.toolEnd('tc1'),
        ev.toolResult('m1', 'tc1', 'found'),
        ev.textStart('m1'),
        ev.text('m1', 'Here is '),
        ev.text('m1', 'the answer.'),
      ]),
    );
    expect(shown(state)).toEqual([{ id: 'm1', body: 'Here is the answer.', provisional: true }]);
  });

  test('replayed_run_error_cleared_by_later_success', () => {
    const events = [
      ev.userStored('u1', 'Check the numbers'),
      ev.runStarted('r1'),
      ev.kwRunStarted('r1', 'dot-a', 'm1'),
      ev.textStart('m1'),
      ev.text('m1', 'Checking'),
      ev.runError('lease_lost'),
      ev.kwRunClosed('r1', 'interrupted'),
      ev.runStarted('r2'),
      ev.kwRunStarted('r2', 'dot-a', 'm1'),
      ev.textStart('m1'),
      ev.text('m1', 'All good'),
      ev.decision('j1'),
      ev.stored('m1', 'The numbers match.'),
      ev.jobStatus('j1', 'answered'),
      ev.dotState('dot-a', 'idle'),
      ev.runFinished('r2'),
      ev.kwRunClosed('r2', 'finished'),
    ];
    const log = numbered(events);
    expect(fold(log.slice(0, 7)).runError).toBe('lease_lost');

    const { straight, resumed } = foldWithReconnect(events, 9, 3);
    for (const state of [straight, resumed]) {
      expect(state.runError).toBeNull();
      expect(shown(state)).toEqual(
        stored(['u1', 'Check the numbers'], ['m1', 'The numbers match.']),
      );
    }
    expect(resumed).toEqual(straight);
  });

  test('interrupt_does_not_clear_run_error', () => {
    const state = fold(
      numbered([
        ev.runStarted('r1'),
        ev.kwRunStarted('r1', 'dot-a', 'm1'),
        ev.dotState('dot-a', 'working'),
        ev.textStart('m1'),
        ev.text('m1', 'Sending'),
        ev.runError('lease_lost'),
        ev.kwRunClosed('r1', 'interrupted'),
        // The task had started a side effect, so it ends interrupted instead of re-running.
        ev.decision('j1'),
        ev.notice('n1', 'interrupted_review_required'),
        ev.jobStatus('j1', 'stopped', 'interrupted_review_required'),
        ev.dotState('dot-a', 'idle'),
      ]),
    );
    expect(state.runError).toBe('lease_lost');
    expect(shown(state)).toEqual(stored(['n1', 'interrupted_review_required']));
  });

  test('snapshot_then_events_after_its_seq', () => {
    const snapshot = transcriptFromSnapshot(
      [
        {
          id: 'u1',
          author: { kind: 'user', id: 'user-1' },
          body: 'Earlier question',
          synthetic: false,
          question: null,
        },
      ],
      4,
    );
    const state = fold(
      [
        { seq: 4, event: ev.userStored('dup', 'already in the snapshot') },
        { seq: 5, event: ev.stored('m1', 'Reply') },
      ],
      snapshot,
    );
    expect(shown(state)).toEqual(stored(['u1', 'Earlier question'], ['m1', 'Reply']));
  });
});

test('parse_author_accepts_tagged_and_serde_default_shapes', () => {
  expect(parseAuthor({ kind: 'dot', id: 'd1' })).toEqual({ kind: 'dot', id: 'd1' });
  expect(parseAuthor({ Dot: 'd1' })).toEqual({ kind: 'dot', id: 'd1' });
  expect(parseAuthor({ User: 'u1' })).toEqual({ kind: 'user', id: 'u1' });
  expect(parseAuthor('System')).toEqual({ kind: 'system' });
  expect(parseAuthor({ kind: 'system' })).toEqual({ kind: 'system' });
  expect(parseAuthor(42)).toBeNull();
});
