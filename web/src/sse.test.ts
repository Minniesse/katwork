import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import {
  emptyTranscript,
  foldEvent,
  mergeEarlier,
  mergeMessages,
  parseAuthor,
  putPosted,
  replyingKats,
  runningJobs,
  STALE_AFTER_MS,
  subscribe,
  transcriptFromSnapshot,
  transcriptMessages,
  type AgUiEvent,
  type SeqEvent,
  type StreamStatus,
  type Transcript,
} from './sse';
import { apiMessage, ev, numbered } from './testing/agui';
import { FakeEventSource, installFakeEventSource } from './testing/fake-event-source';
import { stubFetch } from './testing/fetch';

// Message ids sort in time order, as UUIDv7 ids do: the transcript is ordered by id.
const ASK = '01-ask';
const REPLY_A = '02-reply-a';
const REPLY_B = '03-reply-b';
const NOTICE = '04-notice';
const PATH = '/api/workspaces/w/threads/t/events';

beforeEach(() => {
  installFakeEventSource();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
  vi.restoreAllMocks();
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

function listen(after = 0) {
  const received: SeqEvent[] = [];
  const statuses: StreamStatus[] = [];
  const stop = subscribe(PATH, (e) => received.push(e), {
    after,
    onStatus: (status) => statuses.push(status),
  });
  return { received, statuses, stop };
}

describe('subscribe', () => {
  test('sse_folds_by_id_and_drops_duplicates_after_reconnect', () => {
    const { received, statuses, stop } = listen();
    const source = FakeEventSource.latest();
    expect(source.url).toBe(`${PATH}?after=0`);

    const log = numbered([
      ev.userStored(ASK, 'Hi team'),
      ev.runStarted('rA'),
      ev.kwRunStarted('rA', 'kat-a', REPLY_A),
      ev.runStarted('rB'),
      ev.kwRunStarted('rB', 'kat-b', REPLY_B),
      ev.textStart(REPLY_A),
      ev.textStart(REPLY_B),
      ev.text(REPLY_A, 'สวัสดี'),
      ev.text(REPLY_B, 'Hello'),
      ev.text(REPLY_A, 'ครับ 👋🏽'),
      ev.text(REPLY_B, ' there'),
    ]);
    log.slice(0, 8).forEach((e) => source.emit(e));
    source.drop();
    // The resumed stream overlaps what was already delivered (seq 6 to 8).
    log.slice(5).forEach((e) => source.emit(e));

    expect(received.map((e) => e.seq)).toEqual(log.map((e) => e.seq));
    expect(FakeEventSource.instances).toHaveLength(1);
    expect(statuses).toEqual(['live', 'reconnecting', 'live']);
    expect(shown(fold(received))).toEqual([
      { id: ASK, body: 'Hi team', provisional: false },
      { id: REPLY_A, body: 'สวัสดีครับ 👋🏽', provisional: true },
      { id: REPLY_B, body: 'Hello there', provisional: true },
    ]);

    stop();
    expect(source.readyState).toBe(FakeEventSource.CLOSED);
  });

  test('closed_stream_reopens_with_after_and_skips_replayed_events', async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0.999);
    const fetchMock = stubFetch([200, { user_id: 'user-1' }]);
    const { received, statuses } = listen(4);
    const first = FakeEventSource.latest();
    expect(first.after).toBe('4');
    const log = numbered(
      [ev.userStored(ASK, 'one'), ev.userStored(REPLY_A, 'two'), ev.userStored(REPLY_B, 'three')],
      5,
    );
    first.emit(log[0]!);
    first.emit(log[1]!);
    first.fail();

    await vi.advanceTimersByTimeAsync(990);
    expect(FakeEventSource.instances).toHaveLength(1);
    expect(fetchMock.mock.calls[0]![0]).toBe('/api/auth/me');
    await vi.advanceTimersByTimeAsync(10);
    expect(FakeEventSource.instances).toHaveLength(2);
    const second = FakeEventSource.latest();
    expect(second.after).toBe('6');
    log.forEach((e) => second.emit(e));
    expect(received.map((e) => e.seq)).toEqual([5, 6, 7]);
    expect(statuses).toEqual(['live', 'reconnecting', 'live']);
  });

  test('backoff_doubles_from_1s_to_30s_with_jitter', async () => {
    vi.useFakeTimers();
    stubFetch([200, { user_id: 'user-1' }]);
    const random = vi.spyOn(Math, 'random');
    listen();
    // A wait is drawn between half and all of the ceiling.
    for (const ceiling of [1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000]) {
      random.mockReturnValue(0);
      const count = FakeEventSource.instances.length;
      FakeEventSource.latest().fail();
      await vi.advanceTimersByTimeAsync(ceiling / 2 - 1);
      expect(FakeEventSource.instances, `ceiling ${ceiling}`).toHaveLength(count);
      await vi.advanceTimersByTimeAsync(1);
      expect(FakeEventSource.instances, `ceiling ${ceiling}`).toHaveLength(count + 1);
    }
    random.mockReturnValue(0.9999);
    FakeEventSource.latest().fail();
    await vi.advanceTimersByTimeAsync(29_990);
    const count = FakeEventSource.instances.length;
    await vi.advanceTimersByTimeAsync(10);
    expect(FakeEventSource.instances).toHaveLength(count + 1);
  });

  test('a_frame_resets_the_backoff', async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0);
    stubFetch([200, { user_id: 'user-1' }]);
    listen();
    FakeEventSource.latest().fail();
    await vi.advanceTimersByTimeAsync(500);
    FakeEventSource.latest().fail();
    await vi.advanceTimersByTimeAsync(1_000);
    FakeEventSource.latest().ping();
    FakeEventSource.latest().fail();
    const count = FakeEventSource.instances.length;
    await vi.advanceTimersByTimeAsync(500);
    expect(FakeEventSource.instances).toHaveLength(count + 1);
  });

  test('stop_cancels_a_pending_reopen', async () => {
    vi.useFakeTimers();
    stubFetch([200, { user_id: 'user-1' }]);
    const { stop } = listen();
    FakeEventSource.latest().fail();
    stop();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(FakeEventSource.instances).toHaveLength(1);
  });

  test('dropped_stream_is_a_gentle_reconnect_not_an_error', () => {
    // Task 9's 60 s write deadline drops a slow reader; the browser resumes with Last-Event-ID.
    const { statuses } = listen();
    const source = FakeEventSource.latest();
    source.ping();
    source.drop();
    expect(statuses).toEqual(['live', 'reconnecting']);
    source.ping();
    expect(statuses).toEqual(['live', 'reconnecting', 'live']);
    expect(FakeEventSource.instances).toHaveLength(1);
  });

  test('stale_stream_is_reopened_after_35s_without_a_frame', async () => {
    vi.useFakeTimers();
    const { statuses } = listen(3);
    const first = FakeEventSource.latest();
    first.emit({ seq: 4, event: ev.userStored(ASK, 'hi') });
    await vi.advanceTimersByTimeAsync(STALE_AFTER_MS - 1);
    first.ping();
    await vi.advanceTimersByTimeAsync(STALE_AFTER_MS - 1);
    expect(FakeEventSource.instances).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(first.readyState).toBe(FakeEventSource.CLOSED);
    expect(FakeEventSource.live()).toHaveLength(1);
    expect(FakeEventSource.latest().after).toBe('4');
    expect(statuses).toEqual(['live', 'reconnecting']);
  });

  test('stream_error_unauthenticated_or_not_found_stops', () => {
    const unauthenticated = listen();
    FakeEventSource.latest().named('stream_error', { code: 'unauthenticated' });
    expect(unauthenticated.statuses).toEqual(['live', 'unauthenticated']);
    expect(FakeEventSource.live()).toHaveLength(0);

    const gone = listen();
    FakeEventSource.latest().ping();
    FakeEventSource.latest().named('stream_error', { code: 'not_found' });
    expect(gone.statuses).toEqual(['live']);
    expect(FakeEventSource.live()).toHaveLength(0);
  });

  test('resync_required_reports_resync_and_stops', () => {
    const { statuses } = listen(99);
    FakeEventSource.latest().named('resync_required', { reason: 'cursor_ahead' });
    expect(statuses).toEqual(['live', 'resync']);
    expect(FakeEventSource.live()).toHaveLength(0);
  });

  test('malformed_or_unnumbered_events_are_ignored', () => {
    const { received } = listen();
    const source = FakeEventSource.latest();
    // Before any `id:`, so its lastEventId is empty (an id-less block keeps the previous id).
    source.feed(`data: ${JSON.stringify(ev.userStored(ASK, 'hi'))}\n\n`);
    source.feed('id: 1\ndata: {not json\n\n');
    source.feed(`id: 2\ndata: ${JSON.stringify({ noType: true })}\n\n`);
    source.emit({ seq: 3, event: ev.userStored(ASK, 'hi') });
    expect(received.map((e) => e.seq)).toEqual([3]);
  });
});

describe('foldEvent', () => {
  test('a_duplicate_seq_is_a_no_op', () => {
    const [first] = numbered([ev.userStored(ASK, 'hi')]);
    const once = foldEvent(emptyTranscript(), first!);
    expect(foldEvent(once, first!)).toBe(once);
  });

  test('bubbles_sort_by_message_id', () => {
    // The reply's id was minted at its run's start, before the second question was posted.
    const state = fold(
      numbered([
        ev.userStored(ASK, 'First question'),
        ev.runStarted('r1'),
        ev.kwRunStarted('r1', 'kat-a', REPLY_A),
        ev.textStart(REPLY_A),
        ev.text(REPLY_A, 'Working on it'),
        ev.userStored(REPLY_B, 'A second question'),
      ]),
    );
    expect(shown(state).map((m) => m.id)).toEqual([ASK, REPLY_A, REPLY_B]);
  });

  test('tool_arguments_fold_by_tool_call_id', () => {
    const state = fold(
      numbered([
        ev.runStarted('r1'),
        ev.kwRunStarted('r1', 'kat-a', REPLY_A),
        ev.toolStart('tc1', 'search', REPLY_A),
        ev.toolStart('tc2', 'read_file', REPLY_A),
        ev.toolArgs('tc1', '{"q":'),
        ev.toolArgs('tc2', '{"path":"a.md"}'),
        ev.toolArgs('tc1', '"ภาษีมูลค่าเพิ่ม"}'),
        ev.toolEnd('tc1'),
        ev.toolEnd('tc2'),
        ev.toolResult(REPLY_A, 'tc2', 'contents'),
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

  test('unknown_message_id_delta_is_ignored_not_thrown', () => {
    // Attached mid-run: the TEXT_MESSAGE_START of this message was never seen.
    const state = fold(
      numbered([ev.text('99-unseen', 'half a sentence'), ev.text('99-unseen', ' more')]),
    );
    expect(shown(state)).toEqual([]);
  });

  test('provisional_text_dropped_after_lease_loss_and_recovery', () => {
    const firstRun = numbered([
      ev.userStored(ASK, 'Summarise the report'),
      ev.runStarted('r1'),
      ev.kwRunStarted('r1', 'kat-a', REPLY_A),
      ev.katState('kat-a', 'working'),
      ev.textStart(REPLY_A),
      ev.text(REPLY_A, 'Partial text from the lost run'),
      ev.kwRunClosed('r1', 'interrupted', 'lease_lost'),
      ev.runError('lease_lost'),
    ]);
    const afterLoss = fold(firstRun);
    expect(shown(afterLoss)).toEqual(stored([ASK, 'Summarise the report']));

    const recovery = numbered(
      [
        ev.runStarted('r2'),
        ev.kwRunStarted('r2', 'kat-a', REPLY_A),
        ev.katState('kat-a', 'working'),
        ev.textStart(REPLY_A),
        ev.text(REPLY_A, 'Recovered draft'),
        ev.textEnd(REPLY_A),
        ev.decision('j1'),
        ev.stored(REPLY_A, 'The stored summary.'),
        ev.jobStatus('j1', 'answered'),
        ev.katState('kat-a', 'idle'),
        ev.kwRunClosed('r2', 'finished'),
        ev.runFinished('r2'),
      ],
      firstRun.length + 1,
    );
    const final = fold(recovery, afterLoss);
    expect(shown(final)).toEqual(
      stored([ASK, 'Summarise the report'], [REPLY_A, 'The stored summary.']),
    );
  });

  test('interleaved_runs_one_error_one_stored_reply', () => {
    const log = numbered([
      ev.userStored(ASK, '@Researcher @Writer please help'),
      ev.runStarted('rA'),
      ev.kwRunStarted('rA', 'kat-a', REPLY_A),
      ev.runStarted('rB'),
      ev.kwRunStarted('rB', 'kat-b', REPLY_B),
      ev.textStart(REPLY_A),
      ev.textStart(REPLY_B),
      ev.text(REPLY_A, 'Researching'),
      ev.text(REPLY_B, 'Writing'),
      ev.kwRunClosed('rA', 'interrupted', 'lease_lost'),
      ev.runError('lease_lost'),
    ]);
    const midway = fold(log);
    // Run B's live text stays while run A is closed.
    expect(shown(midway)).toEqual([
      { id: ASK, body: '@Researcher @Writer please help', provisional: false },
      { id: REPLY_B, body: 'Writing', provisional: true },
    ]);

    const rest = numbered(
      [
        ev.text(REPLY_B, ' more'),
        ev.textEnd(REPLY_B),
        ev.decision('jB'),
        ev.stored(REPLY_B, 'The Writer’s stored reply', { author: { kind: 'kat', id: 'kat-b' } }),
        ev.jobStatus('jB', 'answered'),
        ev.kwRunClosed('rB', 'finished'),
        ev.runFinished('rB'),
      ],
      log.length + 1,
    );
    expect(shown(fold(rest, midway))).toEqual(
      stored([ASK, '@Researcher @Writer please help'], [REPLY_B, 'The Writer’s stored reply']),
    );
  });

  test('provisional_text_dropped_after_provider_failure', () => {
    const state = fold(
      numbered([
        ev.userStored(ASK, 'Draft the letter'),
        ev.runStarted('r1'),
        ev.kwRunStarted('r1', 'kat-a', REPLY_A, { jobId: 'j1' }),
        ev.textStart(REPLY_A),
        ev.text(REPLY_A, 'Dear'),
        ev.decision('j1'),
        ev.notice(NOTICE, 'provider_unavailable'),
        ev.jobStatus('j1', 'stopped', 'provider_unavailable'),
        ev.katState('kat-a', 'idle'),
        ev.kwRunClosed('r1', 'finished', 'provider_unavailable'),
        ev.runFinished('r1'),
      ]),
    );
    expect(shown(state)).toEqual(
      stored([ASK, 'Draft the letter'], [NOTICE, 'provider_unavailable']),
    );
    expect(transcriptMessages(state)[1]?.author).toEqual({ kind: 'system' });
    expect(state.jobs['j1']).toMatchObject({
      status: 'stopped',
      reason: 'provider_unavailable',
      holdReason: null,
      katId: 'kat-a',
    });
    expect(state.errors).toEqual({ 'kat-a': 'provider_unavailable' });
  });

  test('final_text_differs_from_deltas_shows_stored_text', () => {
    const state = fold(
      numbered([
        ev.runStarted('r1'),
        ev.kwRunStarted('r1', 'kat-a', REPLY_A),
        ev.textStart(REPLY_A),
        ev.text(REPLY_A, 'Draft answr'),
        ev.textEnd(REPLY_A),
        ev.stored(REPLY_A, 'Final answer.', { synthetic: true }),
        ev.kwRunClosed('r1', 'finished'),
        ev.runFinished('r1'),
      ]),
    );
    expect(transcriptMessages(state)).toEqual([
      expect.objectContaining({
        id: REPLY_A,
        body: 'Final answer.',
        provisional: false,
        synthetic: true,
        author: { kind: 'kat', id: 'kat-a' },
      }),
    ]);
  });

  test('stored_question_carries_its_options', () => {
    const question = { question: 'Which year?', options: ['2569', '2568'] };
    const state = fold(numbered([ev.stored(REPLY_A, 'Which year?', { question })]));
    expect(transcriptMessages(state)[0]?.question).toEqual(question);
  });

  test('new_turn_start_resets_provisional_text', () => {
    const state = fold(
      numbered([
        ev.runStarted('r1'),
        ev.kwRunStarted('r1', 'kat-a', REPLY_A),
        ev.textStart(REPLY_A),
        ev.text(REPLY_A, 'Let me look that up.'),
        ev.textEnd(REPLY_A),
        ev.toolStart('tc1', 'search', REPLY_A),
        ev.toolEnd('tc1'),
        ev.toolResult(REPLY_A, 'tc1', 'found'),
        ev.textStart(REPLY_A),
        ev.text(REPLY_A, 'Here is '),
        ev.text(REPLY_A, 'the answer.'),
      ]),
    );
    expect(shown(state)).toEqual([{ id: REPLY_A, body: 'Here is the answer.', provisional: true }]);
  });

  test('provisional_bubble_takes_synthetic_from_its_run', () => {
    const streaming = fold(
      numbered([
        ev.runStarted('r1'),
        ev.kwRunStarted('r1', 'kat-a', REPLY_A, { synthetic: true }),
        ev.textStart(REPLY_A),
        ev.text(REPLY_A, 'สวัสดี'),
      ]),
    );
    expect(streaming.messages[REPLY_A]).toMatchObject({ provisional: true, synthetic: true });
    const confirmed = foldEvent(streaming, {
      seq: 5,
      event: ev.stored(REPLY_A, 'สวัสดีครับ', { synthetic: false }),
    });
    expect(confirmed.messages[REPLY_A]).toMatchObject({ provisional: false, synthetic: false });
  });

  test('replayed_run_error_cleared_by_later_success', () => {
    const events = [
      ev.userStored(ASK, 'Check the numbers'),
      ev.runStarted('r1'),
      ev.kwRunStarted('r1', 'kat-a', REPLY_A, { jobId: 'j1' }),
      ev.textStart(REPLY_A),
      ev.text(REPLY_A, 'Checking'),
      ev.kwRunClosed('r1', 'interrupted', 'lease_lost'),
      ev.runError('lease_lost'),
      ev.runStarted('r2'),
      ev.kwRunStarted('r2', 'kat-a', REPLY_A, { jobId: 'j1' }),
      ev.textStart(REPLY_A),
      ev.text(REPLY_A, 'All good'),
      ev.decision('j1'),
      ev.stored(REPLY_A, 'The numbers match.'),
      ev.jobStatus('j1', 'answered'),
      ev.katState('kat-a', 'idle'),
      ev.kwRunClosed('r2', 'finished'),
      ev.runFinished('r2'),
    ];
    const log = numbered(events);
    expect(fold(log.slice(0, 7)).errors).toEqual({ 'kat-a': 'lease_lost' });

    const { straight, resumed } = foldWithReconnect(events, 9, 3);
    for (const state of [straight, resumed]) {
      expect(state.errors).toEqual({});
      expect(shown(state)).toEqual(
        stored([ASK, 'Check the numbers'], [REPLY_A, 'The numbers match.']),
      );
    }
    expect(resumed).toEqual(straight);
  });

  test('interrupt_does_not_clear_run_error', () => {
    const state = fold(
      numbered([
        ev.runStarted('r1'),
        ev.kwRunStarted('r1', 'kat-a', REPLY_A, { jobId: 'j1' }),
        ev.katState('kat-a', 'working'),
        ev.textStart(REPLY_A),
        ev.text(REPLY_A, 'Sending'),
        ev.kwRunClosed('r1', 'interrupted', 'lease_lost'),
        ev.runError('lease_lost'),
        // The task had started a side effect, so it ends interrupted instead of re-running.
        ev.decision('j1'),
        ev.notice(NOTICE, 'interrupted_review_required'),
        ev.jobStatus('j1', 'stopped', 'interrupted_review_required'),
        ev.katState('kat-a', 'idle'),
      ]),
    );
    expect(state.errors).toEqual({ 'kat-a': 'lease_lost' });
    expect(shown(state)).toEqual(stored([NOTICE, 'interrupted_review_required']));
  });

  test('errors_are_per_kat_and_do_not_clear_each_other', () => {
    // Kat A's run stops on a provider failure while Kat B's run answers.
    const state = fold(
      numbered([
        ev.userStored(ASK, '@Researcher @Writer'),
        ev.runStarted('rA'),
        ev.kwRunStarted('rA', 'kat-a', REPLY_A, { jobId: 'jA' }),
        ev.runStarted('rB'),
        ev.kwRunStarted('rB', 'kat-b', REPLY_B, { jobId: 'jB' }),
        ev.notice(NOTICE, 'provider_unavailable'),
        ev.jobStatus('jA', 'stopped', 'provider_unavailable'),
        ev.kwRunClosed('rA', 'finished', 'provider_unavailable'),
        ev.runFinished('rA'),
        ev.stored(REPLY_B, 'Done', { author: { kind: 'kat', id: 'kat-b' } }),
        ev.jobStatus('jB', 'answered'),
        ev.kwRunClosed('rB', 'finished'),
        ev.runFinished('rB'),
      ]),
    );
    expect(state.errors).toEqual({ 'kat-a': 'provider_unavailable' });
  });

  test('a_cancelled_or_question_close_fills_no_error_slot', () => {
    const state = fold(
      numbered([
        ev.kwRunStarted('r1', 'kat-a', REPLY_A),
        ev.kwRunClosed('r1', 'cancelled', 'cancelled'),
        ev.runError('cancelled'),
        ev.kwRunStarted('r2', 'kat-b', REPLY_B),
        ev.kwRunClosed('r2', 'finished', 'question_pending'),
        ev.kwRunStarted('r3', 'kat-c', '05-reply-c'),
        ev.kwRunClosed('r3', 'finished', 'approval_pending'),
      ]),
    );
    expect(state.errors).toEqual({});
  });

  test('stopped_reply_keeps_streamed_text_through_run_closed', () => {
    const state = fold(
      numbered([
        ev.runStarted('r1'),
        ev.kwRunStarted('r1', 'kat-a', REPLY_A, { jobId: 'j1' }),
        ev.textStart(REPLY_A),
        ev.text(REPLY_A, 'Hel'),
        ev.text(REPLY_A, 'lo'),
        ev.stored(REPLY_A, 'Hello', { stopped: true, jobId: 'j1' }),
        ev.katState('kat-a', 'idle'),
        ev.kwRunClosed('r1', 'cancelled', 'cancelled'),
        ev.runError('cancelled'),
        ev.jobStatus('j1', 'cancelled', 'cancelled'),
      ]),
    );
    expect(transcriptMessages(state)).toEqual([
      expect.objectContaining({ id: REPLY_A, body: 'Hello', stopped: true, provisional: false }),
    ]);
    expect(state.errors).toEqual({});
    expect(runningJobs(state)).toEqual([]);
  });

  test('job_status_null_marks_a_running_job_and_hold_reason_is_kept', () => {
    const state = fold(
      numbered([
        ev.userStored(ASK, 'Hi', { jobId: 'j1' }),
        ev.jobStatus('j1', null),
        ev.jobStatus('j1', null, null, 'kat_busy'),
      ]),
    );
    expect(runningJobs(state)).toEqual(['j1']);
    expect(state.jobs['j1']).toMatchObject({ holdReason: 'kat_busy', started: false });
    const started = fold(
      numbered([ev.kwRunStarted('r1', 'kat-a', REPLY_A, { jobId: 'j1' })], 4),
      state,
    );
    expect(started.jobs['j1']).toMatchObject({ started: true, katId: 'kat-a' });
  });

  test('snapshot_then_events_after_its_seq', () => {
    const snapshot = transcriptFromSnapshot(
      [
        apiMessage(ASK, 'Earlier question', {
          author: { kind: 'user', id: 'user-1' },
          author_name: 'Person',
        }),
      ],
      4,
    );
    const state = fold(
      [
        { seq: 4, event: ev.userStored('00-dup', 'already in the snapshot') },
        { seq: 5, event: ev.stored(REPLY_A, 'Reply') },
      ],
      snapshot,
    );
    expect(shown(state)).toEqual(stored([ASK, 'Earlier question'], [REPLY_A, 'Reply']));
    expect(state.messages[ASK]).toMatchObject({
      authorName: 'Person',
      createdAt: '2026-10-23T05:00:00Z',
    });
  });

  test('snapshot_job_status_null_offers_stop_after_reload', () => {
    const state = transcriptFromSnapshot(
      [
        apiMessage(ASK, 'Still working?', { author: { kind: 'user', id: 'user-1' }, job_id: 'j1' }),
        apiMessage(REPLY_A, 'Earlier answer', { job_id: 'j0', job_status: 'answered' }),
      ],
      7,
    );
    expect(runningJobs(state)).toEqual(['j1']);
    expect(state.jobs['j0']).toMatchObject({ status: 'answered', katId: 'kat-a' });
  });

  test('posted_message_is_added_once', () => {
    const author = { kind: 'user' as const, id: 'user-1' };
    const fromEvent = fold(
      numbered([ev.userStored(ASK, 'Hi', { clientMsgId: 'c1', jobId: 'j1' })]),
    );
    const posted = { id: ASK, author, body: 'Hi', clientMsgId: 'c1', jobId: 'j1' };
    expect(putPosted(fromEvent, posted)).toBe(fromEvent);
    const fromPost = putPosted(emptyTranscript(), posted);
    expect(shown(fromPost)).toEqual(stored([ASK, 'Hi']));
    expect(runningJobs(fromPost)).toEqual(['j1']);
  });

  test('merge_replaces_by_id_and_takes_rest_job_status', () => {
    const live = fold(
      numbered([ev.userStored(ASK, 'Hi', { jobId: 'j1' }), ev.jobStatus('j1', null)]),
    );
    const merged = mergeMessages(live, [
      apiMessage(ASK, 'Hi', {
        author: { kind: 'user', id: 'user-1' },
        job_id: 'j1',
        job_status: 'answered',
      }),
      apiMessage(REPLY_A, 'Answer', { job_id: 'j1', job_status: 'answered' }),
    ]);
    expect(shown(merged)).toEqual(stored([ASK, 'Hi'], [REPLY_A, 'Answer']));
    expect(runningJobs(merged)).toEqual([]);
  });

  test('mid_run_snapshot_then_interleaved_lease_loss_ends_with_stored_messages', () => {
    const log = numbered([
      ev.userStored(ASK, '@Researcher @Writer please help'),
      ev.runStarted('rA'),
      ev.kwRunStarted('rA', 'kat-a', REPLY_A),
      ev.runStarted('rB'),
      ev.kwRunStarted('rB', 'kat-b', REPLY_B),
      ev.textStart(REPLY_A),
      ev.textStart(REPLY_B),
      ev.text(REPLY_A, 'Researching'),
      ev.text(REPLY_B, 'Writing'),
      ev.text(REPLY_A, ' further'),
      ev.kwRunClosed('rA', 'interrupted', 'lease_lost'),
      ev.runError('lease_lost'),
      ev.text(REPLY_B, ' more'),
      ev.stored(REPLY_B, 'The Writer’s stored reply', { author: { kind: 'kat', id: 'kat-b' } }),
      ev.jobStatus('jB', 'answered'),
      ev.kwRunClosed('rB', 'finished'),
      ev.runFinished('rB'),
    ]);
    // The messages API returns what is stored at some seq; the stream then replays from 0.
    const snapshotAt = (seq: number) =>
      transcriptFromSnapshot(
        transcriptMessages(fold(log.slice(0, seq)))
          .filter((message) => !message.provisional)
          .map(({ id, author, body, synthetic, question }) =>
            apiMessage(id, body, { author, synthetic, question }),
          ),
        seq,
      );

    // Taken after both runs started: text that streams later still belongs to its run.
    const live = fold(log.slice(0, 10), snapshotAt(5));
    expect(live.messages[REPLY_A]).toMatchObject({
      runId: 'rA',
      author: { kind: 'kat', id: 'kat-a' },
    });
    expect(live.messages[REPLY_B]).toMatchObject({
      runId: 'rB',
      author: { kind: 'kat', id: 'kat-b' },
    });

    const final = stored(
      [ASK, '@Researcher @Writer please help'],
      [REPLY_B, 'The Writer’s stored reply'],
    );
    for (let seq = 0; seq <= log.length; seq += 1) {
      expect(shown(fold(log, snapshotAt(seq))), `snapshot at seq ${seq}`).toEqual(final);
    }
  });
});

test('parse_author_reads_the_task_6_shape', () => {
  expect(parseAuthor({ kind: 'kat', id: 'd1' })).toEqual({ kind: 'kat', id: 'd1' });
  expect(parseAuthor({ kind: 'user', id: 'u1' })).toEqual({ kind: 'user', id: 'u1' });
  expect(parseAuthor({ kind: 'system' })).toEqual({ kind: 'system' });
  expect(parseAuthor({ Kat: 'd1' })).toBeNull();
  expect(parseAuthor({ kind: 'kat' })).toBeNull();
  expect(parseAuthor(42)).toBeNull();
});

test('earlier_page_adds_only_what_the_transcript_lacks', () => {
  const live = fold(
    numbered([ev.userStored('05-ask', 'Latest', { jobId: 'j2' }), ev.jobStatus('j2', 'answered')]),
  );
  const merged = mergeEarlier(live, [
    apiMessage('01-old', 'Older', {
      author: { kind: 'user', id: 'user-1' },
      job_id: 'j1',
      job_status: 'answered',
    }),
    apiMessage('05-ask', 'Stale copy', { author: { kind: 'user', id: 'user-1' }, job_id: 'j2' }),
  ]);
  expect(shown(merged)).toEqual(stored(['01-old', 'Older'], ['05-ask', 'Latest']));
  expect(merged.jobs['j2']).toMatchObject({ status: 'answered' });
  expect(merged.jobs['j1']).toMatchObject({ status: 'answered' });
});

test('replying_kats_follow_open_runs', () => {
  const state = fold(
    numbered([
      ev.kwRunStarted('r1', 'kat-a', '01-a'),
      ev.kwRunStarted('r2', 'kat-a', '02-a'),
      ev.kwRunStarted('r3', 'kat-b', '03-b'),
      ev.kwRunClosed('r1', 'finished'),
      ev.kwRunClosed('r3', 'finished'),
    ]),
  );
  expect(replyingKats(state)).toEqual(['kat-a']);
});
