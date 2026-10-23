import { readFileSync } from 'node:fs';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { setLocale } from '../i18n';
import en from '../i18n/en.json';
import th from '../i18n/th.json';
import {
  emptyTranscript,
  foldEvent,
  runningJobs,
  subscribe,
  transcriptMessages,
  type AgUiEvent,
  type ApiMessage,
  type SeqEvent,
} from '../sse';
import { apiMessage, ev } from '../testing/agui';
import { deferred, mockApi, type Reply } from '../testing/api-mock';
import { FakeEventSource, installFakeEventSource } from '../testing/fake-event-source';
import { ME, TEAM, THAI_TEAM, THREAD, THREAD_ROW, WORKSPACE } from '../testing/fixtures';
import { Chat, reduceChat } from './Chat';

const BASE = `/api/workspaces/${WORKSPACE}`;
const MESSAGES = `GET ${BASE}/threads/${THREAD}/messages`;
const POST = `POST ${BASE}/threads/${THREAD}/messages`;
const EVENTS = `${BASE}/threads/${THREAD}/events`;
const cancelRoute = (job: string) => `POST ${BASE}/jobs/${job}/cancel`;
const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

// Ids sort in time order, as UUIDv7 ids do.
const ASK = '01-ask';
const REPLY = '02-reply';
const SECOND = '03-second';
const NOTICE = '04-notice';
const LEAD = 'kat-lead';
const WRITER = 'kat-writer';

beforeEach(() => {
  installFakeEventSource();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
  vi.restoreAllMocks();
  act(() => setLocale('en'));
  Object.defineProperty(document, 'visibilityState', { value: 'visible', configurable: true });
});

const page = (messages: ApiMessage[] = [], eventSeq = 0): Reply => [
  200,
  { messages, event_seq: eventSeq },
];

function api(routes: Record<string, Parameters<typeof mockApi>[0][string]> = {}) {
  return mockApi({
    [`GET ${BASE}/threads/${THREAD}`]: [200, THREAD_ROW],
    [MESSAGES]: page(),
    [`GET ${BASE}/teams/${TEAM}`]: [200, THAI_TEAM],
    'GET /api/auth/me': [200, { user_id: ME }],
    ...routes,
  });
}

function renderChat(props: Partial<Parameters<typeof Chat>[0]> = {}) {
  const onSignedOut = vi.fn();
  const view = render(
    <Chat
      workspaceId={WORKSPACE}
      spaceId="space-1"
      teamId={TEAM}
      threadId={THREAD}
      me={ME}
      onSignedOut={onSignedOut}
      {...props}
    />,
  );
  return { ...view, onSignedOut };
}

/** Renders the open thread and waits for its event stream. */
async function openThread(props: Partial<Parameters<typeof Chat>[0]> = {}) {
  const view = renderChat(props);
  await waitFor(() => expect(FakeEventSource.live()).toHaveLength(1));
  // The team's names arrive with their own request.
  await screen.findByRole('textbox', { name: en['chat.composer_label'] });
  return { ...view, source: FakeEventSource.latest() };
}

/** Emits events with consecutive seqs from `first`, as the server's log numbers them. */
function stream(source: FakeEventSource, first: number, events: AgUiEvent[]) {
  act(() => events.forEach((event, i) => source.emit({ seq: first + i, event })));
  return first + events.length;
}

/** Lets fake time pass while the server keeps the stream alive with its 15 s pings. */
async function pingFor(source: FakeEventSource, ms: number) {
  for (let left = ms; left > 0; left -= 15_000) {
    await act(() => vi.advanceTimersByTimeAsync(Math.min(left, 15_000)));
    act(() => source.ping());
  }
}

// The conversation as a list in message-id order; the log announces only new arrivals.
const list = () => screen.getByRole('list', { name: en['chat.transcript'] });
const log = () => screen.getByRole('log', { name: en['chat.new_messages'] });
const announced = () => [...log().children].map((node) => node.textContent);
// Every bubble on screen: the live log, and the older pages listed above it.
const bodies = () =>
  [...document.querySelectorAll('.transcript .message-body')].map((node) => node.textContent);
const katStatus = () => screen.getByTestId('kat-status');

describe('posting and streaming', () => {
  test('chat_posts_with_client_msg_id_and_streams_the_reply', async () => {
    const posted = deferred<Reply>();
    const calls = api({ [POST]: () => posted.promise });
    const user = userEvent.setup({ delay: null });
    const { source } = await openThread();

    await user.type(
      screen.getByRole('textbox', { name: en['chat.composer_label'] }),
      'สวัสดีครับ{Enter}',
    );
    const [post] = calls.callsTo(POST);
    expect(post!.body).toEqual({
      body: 'สวัสดีครับ',
      client_msg_id: expect.stringMatching(UUID_V4),
    });
    const clientMsgId = (post!.body as { client_msg_id: string }).client_msg_id;
    expect(screen.getByText(en['chat.sending'])).toBeInTheDocument();

    await act(async () => posted.resolve([201, { message_id: ASK, job_id: 'job-1' }]));
    expect(screen.queryByText(en['chat.sending'])).toBeNull();
    expect(bodies()).toEqual(['สวัสดีครับ']);

    stream(source, 1, [
      ev.userStored(ASK, 'สวัสดีครับ', { userId: ME, clientMsgId, jobId: 'job-1' }),
      ev.jobStatus('job-1', null),
      ev.runStarted('run-1'),
      ev.kwRunStarted('run-1', LEAD, REPLY, { jobId: 'job-1' }),
      ev.katState(LEAD, 'working'),
      ev.textStart(REPLY),
      ev.text(REPLY, 'สวัสดี'),
      ev.text(REPLY, 'ค่ะ 👋🏽'),
    ]);
    expect(bodies()).toEqual(['สวัสดีครับ', 'สวัสดีค่ะ 👋🏽']);
    const streaming = screen.getByText('สวัสดีค่ะ 👋🏽').closest('li')!;
    expect(within(streaming).getByText('หัวหน้าทีม')).toBeInTheDocument();

    stream(source, 9, [
      ev.textEnd(REPLY),
      ev.decision('job-1'),
      ev.stored(REPLY, 'สวัสดีค่ะ 👋🏽 มีอะไรให้ช่วยไหมคะ', {
        author: { kind: 'kat', id: LEAD },
        authorName: 'หัวหน้าทีม',
        jobId: 'job-1',
      }),
      ev.jobStatus('job-1', 'answered'),
      ev.katState(LEAD, 'idle'),
      ev.kwRunClosed('run-1', 'finished'),
      ev.runFinished('run-1'),
    ]);
    expect(bodies()).toEqual(['สวัสดีครับ', 'สวัสดีค่ะ 👋🏽 มีอะไรให้ช่วยไหมคะ']);
    expect(within(list()).getAllByRole('listitem')).toHaveLength(2);
    expect(screen.queryByRole('button', { name: en['chat.stop'] })).toBeNull();
    // The person's own post and the finished reply are each announced once.
    expect(announced()).toEqual([
      `${en['chat.author.you']}: สวัสดีครับ`,
      'หัวหน้าทีม: สวัสดีค่ะ 👋🏽 มีอะไรให้ช่วยไหมคะ',
    ]);
  });

  test('optimistic_bubble_rekeys_on_message_stored_before_post_returns', async () => {
    const posted = deferred<Reply>();
    const calls = api({ [POST]: () => posted.promise });
    const user = userEvent.setup({ delay: null });
    const { source } = await openThread();
    await user.type(
      screen.getByRole('textbox', { name: en['chat.composer_label'] }),
      'hello{Enter}',
    );
    const clientMsgId = (calls.callsTo(POST)[0]!.body as { client_msg_id: string }).client_msg_id;

    // The event commits with the message, so it can beat the POST response.
    stream(source, 1, [ev.userStored(ASK, 'hello', { userId: ME, clientMsgId, jobId: 'job-1' })]);
    expect(bodies()).toEqual(['hello']);
    expect(screen.queryByText(en['chat.sending'])).toBeNull();

    await act(async () => posted.resolve([201, { message_id: ASK, job_id: 'job-1' }]));
    expect(bodies()).toEqual(['hello']);
  });

  test('a_failed_post_can_be_retried_with_the_same_client_msg_id', async () => {
    const calls = api({
      [POST]: [
        [503, { code: 'busy' }],
        [201, { message_id: ASK, job_id: 'job-1' }],
      ],
    });
    const user = userEvent.setup({ delay: null });
    await openThread();
    await user.type(
      screen.getByRole('textbox', { name: en['chat.composer_label'] }),
      'hello{Enter}',
    );
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(en['chat.not_sent']);
    expect(alert).toHaveTextContent(en['reason.busy']);
    await user.click(within(alert).getByRole('button', { name: en['chat.retry'] }));
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    const [first, second] = calls.callsTo(POST);
    expect(second!.body).toEqual(first!.body);
    expect(bodies()).toEqual(['hello']);
  });

  test('second_tab_sees_user_message_from_event', async () => {
    api();
    const { source } = await openThread();
    stream(source, 1, [
      ev.userStored(ASK, 'ขอสรุปประชุมหน่อย', {
        userId: 'user-2',
        authorName: 'สมชาย',
        jobId: 'job-1',
      }),
      ev.jobStatus('job-1', null),
      ev.userStored(SECOND, 'From my other tab', { userId: ME, authorName: 'Pat', jobId: 'job-2' }),
    ]);
    const [theirs, mine] = within(list()).getAllByRole('listitem');
    expect(within(theirs!).getByText('สมชาย')).toBeVisible();
    expect(within(mine!).getByText(en['chat.author.you'])).toBeVisible();
    // Another member's job is shown as work in progress, without Stop.
    expect(screen.queryByRole('button', { name: en['chat.stop'] })).toBeNull();
    expect(katStatus()).toHaveTextContent(en['chat.others_working']);
  });

  test('send_works_without_crypto_random_uuid', async () => {
    // A plain-HTTP LAN address is not a secure context, so randomUUID is missing there.
    Object.defineProperty(crypto, 'randomUUID', { value: undefined, configurable: true });
    try {
      const calls = api({ [POST]: [201, { message_id: ASK, job_id: 'job-1' }] });
      const user = userEvent.setup({ delay: null });
      await openThread();
      await user.type(
        screen.getByRole('textbox', { name: en['chat.composer_label'] }),
        'hello from the LAN{Enter}',
      );
      const [post] = calls.callsTo(POST);
      expect(post!.body).toEqual({
        body: 'hello from the LAN',
        client_msg_id: expect.stringMatching(UUID_V4),
      });
      await waitFor(() => expect(bodies()).toEqual(['hello from the LAN']));
    } finally {
      delete (crypto as { randomUUID?: unknown }).randomUUID;
    }
    expect(typeof crypto.randomUUID).toBe('function');
  });

  test('message_posted_during_run_orders_same_live_and_after_reload', async () => {
    api();
    const { source, unmount } = await openThread();
    stream(source, 1, [
      ev.userStored(ASK, 'Draft the memo', { userId: ME, jobId: 'job-1' }),
      ev.runStarted('run-1'),
      ev.kwRunStarted('run-1', LEAD, REPLY, { jobId: 'job-1' }),
      ev.textStart(REPLY),
      ev.text(REPLY, 'Drafting'),
      // Posted while the reply streams: its id is newer than the reply's, minted at run start.
      ev.userStored(SECOND, 'Make it short', { userId: ME, jobId: 'job-2' }),
    ]);
    expect(bodies()).toEqual(['Draft the memo', 'Drafting', 'Make it short']);
    stream(source, 7, [ev.stored(REPLY, 'The memo.', { author: { kind: 'kat', id: LEAD } })]);
    const live = bodies();
    expect(live).toEqual(['Draft the memo', 'The memo.', 'Make it short']);

    unmount();
    api({
      [MESSAGES]: page(
        [
          apiMessage(ASK, 'Draft the memo', { author: { kind: 'user', id: ME } }),
          apiMessage(REPLY, 'The memo.', { author: { kind: 'kat', id: LEAD } }),
          apiMessage(SECOND, 'Make it short', { author: { kind: 'user', id: ME } }),
        ],
        7,
      ),
    });
    await openThread();
    expect(bodies()).toEqual(live);
  });

  test('unknown_message_id_delta_is_ignored_not_thrown', async () => {
    api();
    const { source } = await openThread();
    stream(source, 1, [ev.text('09-unseen', 'half a reply'), ev.userStored(ASK, 'still works')]);
    expect(bodies()).toEqual(['still works']);
  });
});

describe('the event stream', () => {
  test('fresh_load_connects_with_after_param', async () => {
    api({
      [MESSAGES]: page([apiMessage(ASK, 'Earlier', { author: { kind: 'user', id: ME } })], 41),
    });
    const { source } = await openThread();
    expect(source.url).toBe(`${EVENTS}?after=41`);
    expect(bodies()).toEqual(['Earlier']);
  });

  test('reconnects_after_closed_with_after_param_and_no_duplicates', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.spyOn(Math, 'random').mockReturnValue(0.999);
    api({ [MESSAGES]: page([], 3) });
    const { source } = await openThread();
    stream(source, 4, [ev.userStored(ASK, 'one'), ev.userStored(REPLY, 'two')]);
    act(() => source.fail());
    // A gentle state, not an error.
    expect(screen.getByText(en['chat.reconnecting'])).toBeVisible();
    expect(screen.queryByRole('alert')).toBeNull();

    await act(() => vi.advanceTimersByTimeAsync(1_000));
    expect(FakeEventSource.instances).toHaveLength(2);
    const reopened = FakeEventSource.latest();
    expect(reopened.url).toBe(`${EVENTS}?after=5`);
    stream(reopened, 4, [
      ev.userStored(ASK, 'one'),
      ev.userStored(REPLY, 'two'),
      ev.userStored(SECOND, 'three'),
    ]);
    expect(bodies()).toEqual(['one', 'two', 'three']);
    expect(screen.queryByText(en['chat.reconnecting'])).toBeNull();
  });

  test('closed_stream_with_401_routes_to_login', async () => {
    api({ 'GET /api/auth/me': [401, { code: 'unauthenticated' }] });
    const { source, onSignedOut } = await openThread();
    act(() => source.fail());
    await waitFor(() => expect(onSignedOut).toHaveBeenCalledOnce());
    expect(FakeEventSource.instances).toHaveLength(1);
  });

  test('stream_error_unauthenticated_routes_to_login', async () => {
    api();
    const { source, onSignedOut } = await openThread();
    act(() => source.named('stream_error', { code: 'unauthenticated' }));
    expect(onSignedOut).toHaveBeenCalledOnce();
    expect(FakeEventSource.live()).toHaveLength(0);
  });

  test('fifth_tab_falls_back_to_polling_and_resumes_streaming', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const calls = api({
      [MESSAGES]: [
        page([], 2),
        page(
          [
            apiMessage(ASK, 'Posted elsewhere', {
              author: { kind: 'user', id: 'user-2' },
              author_name: 'สมชาย',
            }),
          ],
          3,
        ),
      ],
    });
    const { source } = await openThread();
    act(() => source.named('stream_error', { code: 'too_many_streams' }));
    expect(screen.getByText(en['chat.polling'])).toBeVisible();
    expect(calls.callsTo(MESSAGES)).toHaveLength(1);

    await act(() => vi.advanceTimersByTimeAsync(5_000));
    expect(calls.callsTo(MESSAGES)).toHaveLength(2);
    await waitFor(() => expect(bodies()).toEqual(['Posted elsewhere']));
    await act(() => vi.advanceTimersByTimeAsync(5_000));
    expect(calls.callsTo(MESSAGES)).toHaveLength(3);

    // The server ended that stream; the EventSource's own retry comes 30 s later.
    act(() => source.drop());
    await act(() => vi.advanceTimersByTimeAsync(30_000));
    expect(screen.getByText(en['chat.polling'])).toBeVisible();
    expect(FakeEventSource.instances).toHaveLength(1);
    act(() => source.ping(3));
    expect(screen.queryByText(en['chat.polling'])).toBeNull();
    const polled = calls.callsTo(MESSAGES).length;
    await act(() => vi.advanceTimersByTimeAsync(15_000));
    expect(calls.callsTo(MESSAGES)).toHaveLength(polled);
  });

  test('a_poll_answered_after_the_stream_resumes_never_rolls_back_state', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const held = deferred<Reply>();
    const running = page(
      [apiMessage(ASK, 'Write it', { author: { kind: 'user', id: ME }, job_id: 'job-1' })],
      2,
    );
    const calls = api();
    let reads = 0;
    calls.route(MESSAGES, () => (reads++ === 0 ? running : held.promise));
    const { source } = await openThread();
    expect(screen.getByRole('button', { name: en['chat.stop'] })).toBeVisible();
    act(() => source.named('stream_error', { code: 'too_many_streams' }));
    await act(() => vi.advanceTimersByTimeAsync(5_000));
    expect(calls.callsTo(MESSAGES)).toHaveLength(2);

    // The stream comes back and the job finishes before the old poll answers.
    act(() => source.ping(2));
    stream(source, 3, [ev.jobStatus('job-1', 'answered')]);
    expect(screen.queryByRole('button', { name: en['chat.stop'] })).toBeNull();
    await act(async () => held.resolve(running));
    expect(screen.queryByRole('button', { name: en['chat.stop'] })).toBeNull();
  });

  test('a_poll_answered_after_a_resync_never_overwrites_the_new_snapshot', async () => {
    // A restored database rewinds the seq, so only dropping the polls of a page that stopped
    // polling keeps this old answer out.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const held = deferred<Reply>();
    const asked = apiMessage(ASK, 'Write it', {
      author: { kind: 'user', id: ME },
      job_id: 'job-1',
    });
    const replies: (Reply | Promise<Reply>)[] = [
      page([asked], 10),
      held.promise,
      page([{ ...asked, job_status: 'answered' }], 4),
    ];
    const calls = api();
    calls.route(MESSAGES, () => replies.shift() ?? page([], 4));
    const { source } = await openThread();
    act(() => source.named('stream_error', { code: 'too_many_streams' }));
    await act(() => vi.advanceTimersByTimeAsync(5_000));
    expect(calls.callsTo(MESSAGES)).toHaveLength(2);

    act(() => source.named('resync_required', { reason: 'cursor_ahead' }));
    await waitFor(() => expect(FakeEventSource.latest().url).toBe(`${EVENTS}?after=4`));
    expect(screen.queryByRole('button', { name: en['chat.stop'] })).toBeNull();
    await act(async () => held.resolve(page([asked], 10)));
    expect(screen.queryByRole('button', { name: en['chat.stop'] })).toBeNull();
  });

  test('an_older_poll_never_replaces_a_newer_one', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const held = deferred<Reply>();
    const asked = apiMessage(ASK, 'Write it', {
      author: { kind: 'user', id: ME },
      job_id: 'job-1',
    });
    const replies: (Reply | Promise<Reply>)[] = [
      page([asked], 2),
      held.promise,
      page(
        [
          { ...asked, job_status: 'answered' },
          apiMessage(REPLY, 'Done', { job_id: 'job-1', job_status: 'answered' }),
        ],
        9,
      ),
    ];
    const calls = api();
    calls.route(MESSAGES, () => replies.shift()!);
    const { source } = await openThread();
    act(() => source.named('stream_error', { code: 'too_many_streams' }));
    await act(() => vi.advanceTimersByTimeAsync(5_000));
    await act(() => vi.advanceTimersByTimeAsync(5_000));
    await waitFor(() => expect(bodies()).toEqual(['Write it', 'Done']));
    expect(screen.queryByRole('button', { name: en['chat.stop'] })).toBeNull();
    // The first poll, sent earlier, answers last with the job still running.
    await act(async () => held.resolve(page([asked], 2)));
    expect(screen.queryByRole('button', { name: en['chat.stop'] })).toBeNull();
    expect(bodies()).toEqual(['Write it', 'Done']);
  });

  test('chat_opened_in_a_hidden_tab_gives_its_stream_back_and_starts_when_visible', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true });
    api({ [MESSAGES]: page([], 6) });
    const { source } = await openThread();
    await pingFor(source, 59_000);
    expect(FakeEventSource.live()).toHaveLength(1);
    await act(() => vi.advanceTimersByTimeAsync(1_000));
    expect(FakeEventSource.live()).toHaveLength(0);
    act(() => {
      Object.defineProperty(document, 'visibilityState', { value: 'visible', configurable: true });
      document.dispatchEvent(new Event('visibilitychange'));
    });
    await waitFor(() => expect(FakeEventSource.live()).toHaveLength(1));
    expect(FakeEventSource.latest().url).toBe(`${EVENTS}?after=6`);
  });

  test('a_hidden_tab_stops_polling_and_resumes_when_visible', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const calls = api({ [MESSAGES]: page([], 2) });
    const { source } = await openThread();
    act(() => source.named('stream_error', { code: 'too_many_streams' }));
    act(() => {
      Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true });
      document.dispatchEvent(new Event('visibilitychange'));
    });
    await act(() => vi.advanceTimersByTimeAsync(60_000));
    expect(FakeEventSource.live()).toHaveLength(0);
    expect(screen.queryByText(en['chat.polling'])).toBeNull();
    const polled = calls.callsTo(MESSAGES).length;
    await act(() => vi.advanceTimersByTimeAsync(30_000));
    expect(calls.callsTo(MESSAGES)).toHaveLength(polled);

    act(() => {
      Object.defineProperty(document, 'visibilityState', { value: 'visible', configurable: true });
      document.dispatchEvent(new Event('visibilitychange'));
    });
    await waitFor(() => expect(FakeEventSource.live()).toHaveLength(1));
    act(() => FakeEventSource.latest().ping(2));
    await act(() => vi.advanceTimersByTimeAsync(10_000));
    expect(calls.callsTo(MESSAGES)).toHaveLength(polled);
  });

  test('stale_ping_triggers_reconnect', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    api({ [MESSAGES]: page([], 7) });
    const { source } = await openThread();
    stream(source, 8, [ev.userStored(ASK, 'hi')]);
    await act(() => vi.advanceTimersByTimeAsync(34_000));
    act(() => source.ping(8));
    await act(() => vi.advanceTimersByTimeAsync(34_000));
    expect(FakeEventSource.instances).toHaveLength(1);
    await act(() => vi.advanceTimersByTimeAsync(1_000));
    expect(source.readyState).toBe(FakeEventSource.CLOSED);
    expect(FakeEventSource.live()).toHaveLength(1);
    expect(FakeEventSource.latest().url).toBe(`${EVENTS}?after=8`);
  });

  test('resync_required_reloads_the_snapshot', async () => {
    const calls = api({
      [MESSAGES]: [
        page([apiMessage(ASK, 'Before the restore', { author: { kind: 'user', id: ME } })], 50),
        page(
          [
            apiMessage(ASK, 'Before the restore', { author: { kind: 'user', id: ME } }),
            apiMessage(REPLY, 'Restored reply'),
          ],
          12,
        ),
      ],
    });
    const { source } = await openThread();
    act(() => source.named('resync_required', { reason: 'cursor_ahead' }));
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(2));
    expect(calls.callsTo(MESSAGES)).toHaveLength(2);
    expect(FakeEventSource.latest().url).toBe(`${EVENTS}?after=12`);
    expect(FakeEventSource.live()).toHaveLength(1);
    await waitFor(() => expect(bodies()).toEqual(['Before the restore', 'Restored reply']));
  });

  test('dropped_stream_shows_gentle_reconnecting_line', async () => {
    api();
    const { source } = await openThread();
    act(() => source.ping());
    act(() => source.drop());
    const line = screen.getByText(en['chat.reconnecting']);
    expect(line.closest('[role="status"]')).not.toBeNull();
    expect(screen.queryByRole('alert')).toBeNull();
    act(() => source.ping());
    expect(screen.queryByText(en['chat.reconnecting'])).toBeNull();
  });

  test('leaving_chat_closes_stream', async () => {
    api();
    const { source, unmount } = await openThread();
    unmount();
    expect(source.readyState).toBe(FakeEventSource.CLOSED);
    expect(FakeEventSource.live()).toHaveLength(0);
  });

  test('strict_mode_double_mount_leaves_one_open_stream', async () => {
    api();
    render(
      <StrictMode>
        <Chat
          workspaceId={WORKSPACE}
          spaceId="space-1"
          teamId={TEAM}
          threadId={THREAD}
          me={ME}
          onSignedOut={() => {}}
        />
      </StrictMode>,
    );
    await screen.findByRole('textbox', { name: en['chat.composer_label'] });
    await waitFor(() => expect(FakeEventSource.instances.length).toBeGreaterThan(0));
    expect(FakeEventSource.live()).toHaveLength(1);
  });

  test('hidden_page_closes_stream_after_60s_and_resumes_with_after', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    api({ [MESSAGES]: page([], 4) });
    const { source } = await openThread();
    stream(source, 5, [ev.userStored(ASK, 'one'), ev.userStored(REPLY, 'two')]);
    const visibility = (value: 'hidden' | 'visible') =>
      act(() => {
        Object.defineProperty(document, 'visibilityState', { value, configurable: true });
        document.dispatchEvent(new Event('visibilitychange'));
      });

    visibility('hidden');
    await pingFor(source, 59_000);
    expect(source.readyState).not.toBe(FakeEventSource.CLOSED);
    await act(() => vi.advanceTimersByTimeAsync(1_000));
    expect(FakeEventSource.live()).toHaveLength(0);
    await act(() => vi.advanceTimersByTimeAsync(60_000));
    expect(FakeEventSource.instances).toEqual([source]);

    visibility('visible');
    await waitFor(() => expect(FakeEventSource.live()).toHaveLength(1));
    expect(FakeEventSource.latest().url).toBe(`${EVENTS}?after=6`);
    expect(bodies()).toEqual(['one', 'two']);
  });

  test('briefly_hidden_page_keeps_its_stream', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    api();
    const { source } = await openThread();
    act(() => {
      Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true });
      document.dispatchEvent(new Event('visibilitychange'));
    });
    await pingFor(source, 30_000);
    act(() => {
      Object.defineProperty(document, 'visibilityState', { value: 'visible', configurable: true });
      document.dispatchEvent(new Event('visibilitychange'));
    });
    await pingFor(source, 60_000);
    expect(FakeEventSource.instances).toEqual([source]);
    expect(source.readyState).not.toBe(FakeEventSource.CLOSED);
  });
});

test('utf8_fixture_every_byte_split_reaches_one_bubble', async () => {
  const bytes = new Uint8Array(readFileSync('src/testing/fixtures/thai_emoji.sse'));
  const text = new TextDecoder().decode(bytes);
  const source = JSON.parse(/"delta":("(?:[^"\\]|\\.)*")/.exec(text)![1]!) as string;
  expect(source).toBe('ที่นี่สวัสดีครับ 👋🏽 🇹🇭');

  // Every byte offset, through a streaming TextDecoder, eventsource-parser and FakeEventSource.
  for (let cut = 0; cut <= bytes.length; cut += 1) {
    const received: SeqEvent[] = [];
    const stop = subscribe(EVENTS, (event) => received.push(event), { after: 0 });
    const fake = FakeEventSource.latest();
    fake.feedBytes(bytes.subarray(0, cut));
    fake.feedBytes(bytes.subarray(cut));
    stop();
    const bubbles = transcriptMessages(received.reduce(foldEvent, emptyTranscript()));
    expect(
      bubbles.map((bubble) => bubble.body),
      `cut at byte ${cut}`,
    ).toEqual([source]);
  }

  // And one byte at a time into the Chat page itself.
  api();
  const { source: page } = await openThread();
  act(() => bytes.forEach((byte) => page.feedBytes(Uint8Array.of(byte))));
  expect(bodies()).toEqual([source]);
});

describe('Stop', () => {
  const running = (source: FakeEventSource) =>
    stream(source, 1, [
      ev.userStored(ASK, 'Write the report', { userId: ME, jobId: 'job-1' }),
      ev.jobStatus('job-1', null),
      ev.runStarted('run-1'),
      ev.kwRunStarted('run-1', LEAD, REPLY, { jobId: 'job-1' }),
      ev.katState(LEAD, 'working'),
      ev.textStart(REPLY),
      ev.text(REPLY, 'Hel'),
      ev.text(REPLY, 'lo'),
    ]);

  test('stop_button_cancels_running_job', async () => {
    const calls = api({ [cancelRoute('job-1')]: [202] });
    const user = userEvent.setup({ delay: null });
    const { source } = await openThread();
    expect(screen.queryByRole('button', { name: en['chat.stop'] })).toBeNull();
    running(source);
    await user.click(screen.getByRole('button', { name: en['chat.stop'] }));
    expect(calls.callsTo(cancelRoute('job-1'))).toHaveLength(1);
  });

  test('stop_cancels_only_the_signed_in_persons_jobs', async () => {
    const calls = api({ [cancelRoute('job-mine')]: [202], [cancelRoute('job-theirs')]: [202] });
    const user = userEvent.setup({ delay: null });
    const { source } = await openThread();
    stream(source, 1, [
      ev.userStored(ASK, '@นักเขียน draft the memo', { userId: 'user-2', jobId: 'job-theirs' }),
      ev.jobStatus('job-theirs', null),
      ev.kwRunStarted('run-theirs', WRITER, REPLY, { jobId: 'job-theirs' }),
      ev.userStored(SECOND, 'Summarise the budget', { userId: ME, jobId: 'job-mine' }),
      ev.jobStatus('job-mine', null),
    ]);
    await user.click(screen.getByRole('button', { name: en['chat.stop'] }));
    expect(calls.callsTo(cancelRoute('job-mine'))).toHaveLength(1);
    expect(calls.callsTo(cancelRoute('job-theirs'))).toHaveLength(0);

    // Once mine is done, the colleague's work stays visible, read-only.
    stream(source, 6, [ev.jobStatus('job-mine', 'cancelled', 'cancelled')]);
    expect(screen.queryByRole('button', { name: en['chat.stop'] })).toBeNull();
    expect(katStatus()).toHaveTextContent(en['chat.others_working']);
  });

  test('stop_enabled_after_reload_from_job_status_null', async () => {
    const calls = api({
      [MESSAGES]: page(
        [apiMessage(ASK, 'Queued request', { author: { kind: 'user', id: ME }, job_id: 'job-9' })],
        3,
      ),
      [cancelRoute('job-9')]: [202],
    });
    const user = userEvent.setup({ delay: null });
    await openThread();
    await user.click(screen.getByRole('button', { name: en['chat.stop'] }));
    expect(calls.callsTo(cancelRoute('job-9'))).toHaveLength(1);
  });

  test('stop_409_job_already_finished_is_silent', async () => {
    api({ [cancelRoute('job-1')]: [409, { code: 'job_already_finished' }] });
    const user = userEvent.setup({ delay: null });
    const { source } = await openThread();
    running(source);
    await user.click(screen.getByRole('button', { name: en['chat.stop'] }));
    await waitFor(() => expect(screen.queryByRole('button', { name: en['chat.stop'] })).toBeNull());
    expect(screen.queryByRole('alert')).toBeNull();
  });

  test('stop_double_click_posts_once', async () => {
    const held = deferred<Reply>();
    const calls = api({ [cancelRoute('job-1')]: () => held.promise });
    const user = userEvent.setup({ delay: null });
    const { source } = await openThread();
    running(source);
    const stop = screen.getByRole('button', { name: en['chat.stop'] });
    await user.dblClick(stop);
    await user.click(stop);
    expect(calls.callsTo(cancelRoute('job-1'))).toHaveLength(1);
    expect(stop).toHaveTextContent(en['chat.stopping']);
    expect(stop).toBeDisabled();
    await act(async () => held.resolve([202]));
  });

  test('a_failed_stop_says_so_and_can_be_tried_again', async () => {
    const calls = api({ [cancelRoute('job-1')]: [[503, { code: 'busy' }], [202]] });
    const user = userEvent.setup({ delay: null });
    const { source } = await openThread();
    running(source);
    await user.click(screen.getByRole('button', { name: en['chat.stop'] }));
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(en['chat.stop_failed']);
    await user.click(screen.getByRole('button', { name: en['chat.stop'] }));
    expect(calls.callsTo(cancelRoute('job-1'))).toHaveLength(2);
  });

  test('cancelled_close_is_neutral_not_an_error', async () => {
    api({ [cancelRoute('job-1')]: [202] });
    const { source } = await openThread();
    const next = running(source);
    stream(source, next, [
      ev.stored(REPLY, 'Hello', {
        author: { kind: 'kat', id: LEAD },
        stopped: true,
        jobId: 'job-1',
      }),
      ev.katState(LEAD, 'idle'),
      ev.kwRunClosed('run-1', 'cancelled', 'cancelled'),
      ev.runError('cancelled'),
      ev.jobStatus('job-1', 'cancelled', 'cancelled'),
    ]);
    expect(katStatus()).toHaveTextContent('');
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.queryByRole('button', { name: en['chat.stop'] })).toBeNull();
  });

  test('stop_keeps_partial_text_marked_stopped', async () => {
    api({ [cancelRoute('job-1')]: [202] });
    const user = userEvent.setup({ delay: null });
    const { source, unmount } = await openThread();
    const next = running(source);
    expect(bodies()).toEqual(['Write the report', 'Hello']);
    await user.click(screen.getByRole('button', { name: en['chat.stop'] }));

    stream(source, next, [
      ev.stored(REPLY, 'Hello', {
        author: { kind: 'kat', id: LEAD },
        stopped: true,
        jobId: 'job-1',
      }),
      ev.katState(LEAD, 'idle'),
      ev.kwRunClosed('run-1', 'cancelled', 'cancelled'),
      ev.runError('cancelled'),
      ev.jobStatus('job-1', 'cancelled', 'cancelled'),
    ]);
    expect(bodies()).toEqual(['Write the report', 'Hello']);
    const reply = () => screen.getByText('Hello').closest('li')!;
    expect(within(reply()).getByText(en['chat.stopped_label'])).toBeVisible();
    act(() => setLocale('th'));
    expect(within(reply()).getByText(th['chat.stopped_label'])).toBeVisible();

    unmount();
    act(() => setLocale('en'));
    api({
      [MESSAGES]: page(
        [
          apiMessage(ASK, 'Write the report', {
            author: { kind: 'user', id: ME },
            job_id: 'job-1',
            job_status: 'cancelled',
            job_reason: 'cancelled',
          }),
          apiMessage(REPLY, 'Hello', {
            author: { kind: 'kat', id: LEAD },
            stopped: true,
            job_id: 'job-1',
            job_status: 'cancelled',
            job_reason: 'cancelled',
          }),
        ],
        20,
      ),
    });
    await openThread();
    expect(bodies()).toEqual(['Write the report', 'Hello']);
    expect(within(reply()).getByText(en['chat.stopped_label'])).toBeVisible();
    expect(screen.queryByRole('button', { name: en['chat.stop'] })).toBeNull();
  });
});

describe('questions', () => {
  const question = { question: 'Which format do you want?', options: ['PDF', 'Word', 'Slides'] };
  const asked = (overrides: Partial<ApiMessage> = {}) =>
    apiMessage(REPLY, question.question, {
      author: { kind: 'kat', id: WRITER },
      author_name: 'นักเขียน',
      question,
      job_id: 'job-q',
      job_status: 'needs_input',
      job_reason: 'question_pending',
      ...overrides,
    });

  test('clicking_an_option_sends_it_as_the_answer', async () => {
    const posted = deferred<Reply>();
    const calls = api({
      [MESSAGES]: page(
        [apiMessage(ASK, 'Draft the brief', { author: { kind: 'user', id: ME } }), asked()],
        9,
      ),
      [POST]: () => posted.promise,
    });
    const user = userEvent.setup({ delay: null });
    await openThread();
    const group = screen.getByRole('group', { name: en['chat.question_options'] });
    const word = within(group).getByRole('button', { name: 'Word' });
    await user.click(word);
    // Focus leaves the now disabled options for the composer, not the page body.
    expect(screen.getByRole('textbox', { name: en['chat.composer_label'] })).toHaveFocus();
    await user.click(word);
    const posts = calls.callsTo(POST);
    expect(posts).toHaveLength(1);
    expect(posts[0]!.body).toEqual({ body: 'Word', client_msg_id: expect.stringMatching(UUID_V4) });
    for (const option of within(group).getAllByRole('button')) expect(option).toBeDisabled();
    // The answer shows as the person's message.
    const answer = within(list())
      .getAllByText('Word')
      .map((node) => node.closest('li')!)
      .at(-1)!;
    expect(answer).toHaveClass('message-user');
    expect(within(answer).getByText(en['chat.author.you'])).toBeInTheDocument();
    await act(async () => posted.resolve([201, { message_id: SECOND, job_id: 'job-follow-up' }]));
    expect(bodies().at(-1)).toBe('Word');
  });

  test('stale_question_options_are_disabled', async () => {
    api({
      [MESSAGES]: page(
        [
          asked(),
          apiMessage(SECOND, 'Never mind, I found it', { author: { kind: 'user', id: ME } }),
        ],
        9,
      ),
    });
    const { unmount } = await openThread();
    // No longer the latest message: read-only.
    expect(screen.queryByRole('group', { name: en['chat.question_options'] })).toBeNull();
    expect(screen.getByText('Word')).toBeVisible();
    unmount();

    api({ [MESSAGES]: page([asked({ job_status: 'stopped', job_reason: 'time_limit' })], 9) });
    const { source } = await openThread();
    // Its job no longer waits on the question: read-only.
    expect(screen.queryByRole('group', { name: en['chat.question_options'] })).toBeNull();

    // A live question becomes actionable, then read-only once its job moves on.
    stream(source, 10, [
      ev.stored(NOTICE, 'Which one?', {
        author: { kind: 'kat', id: WRITER },
        question,
        jobId: 'job-q2',
      }),
      ev.jobStatus('job-q2', 'needs_input', 'question_pending'),
    ]);
    expect(screen.getByRole('group', { name: en['chat.question_options'] })).toBeVisible();
    stream(source, 12, [ev.jobStatus('job-q2', 'cancelled', 'cancelled')]);
    expect(screen.queryByRole('group', { name: en['chat.question_options'] })).toBeNull();
  });
});

describe('Kat status', () => {
  test('synthetic_badge_shows_while_streaming', async () => {
    api();
    const { source } = await openThread();
    stream(source, 1, [
      ev.runStarted('run-1'),
      ev.kwRunStarted('run-1', LEAD, REPLY, { synthetic: true }),
      ev.textStart(REPLY),
      ev.text(REPLY, 'A sample'),
    ]);
    // Found by its place in the list, which keeps working after the locale switch.
    const bubble = () =>
      [...document.querySelectorAll('.transcript-list .message-body')]
        .find((node) => /A sample/.test(node.textContent ?? ''))!
        .closest('li')!;
    expect(within(bubble()).getByText(en['chat.synthetic_badge'])).toBeInTheDocument();
    act(() => setLocale('th'));
    expect(within(bubble()).getByText(th['chat.synthetic_badge'])).toBeInTheDocument();
    act(() => setLocale('en'));

    stream(source, 5, [
      ev.stored(REPLY, 'A sample reply', { author: { kind: 'kat', id: LEAD }, synthetic: false }),
    ]);
    expect(within(bubble()).queryByText(en['chat.synthetic_badge'])).toBeNull();

    stream(source, 6, [
      ev.runStarted('run-2'),
      ev.kwRunStarted('run-2', LEAD, NOTICE, { synthetic: false }),
      ev.textStart(NOTICE),
      ev.text(NOTICE, 'A real reply'),
    ]);
    expect(
      within(within(list()).getByText('A real reply').closest('li')!).queryByText(
        en['chat.synthetic_badge'],
      ),
    ).toBeNull();
  });

  test('errors_are_per_kat_and_do_not_clear_each_other', async () => {
    api();
    const { source } = await openThread();
    const providerDown = en['chat.kat_error']
      .replace('{kat}', 'หัวหน้าทีม')
      .replace('{message}', en['reason.provider_unavailable']);
    const writerTimeout = en['chat.kat_error']
      .replace('{kat}', 'นักเขียน')
      .replace('{message}', en['reason.time_limit']);

    // The pipeline's sequence: the Lead's run stops on a provider failure, the Writer's answers.
    let seq = stream(source, 1, [
      ev.userStored(ASK, '@นักเขียน ช่วยด้วย', { userId: ME, jobId: 'job-a' }),
      ev.runStarted('run-a'),
      ev.kwRunStarted('run-a', LEAD, REPLY, { jobId: 'job-a' }),
      ev.runStarted('run-b'),
      ev.kwRunStarted('run-b', WRITER, SECOND, { jobId: 'job-b' }),
      ev.notice(NOTICE, 'provider_unavailable', [{ id: LEAD, name: 'หัวหน้าทีม' }]),
      ev.jobStatus('job-a', 'stopped', 'provider_unavailable'),
      ev.kwRunClosed('run-a', 'finished', 'provider_unavailable'),
      ev.runFinished('run-a'),
      ev.stored(SECOND, 'เสร็จแล้ว', { author: { kind: 'kat', id: WRITER }, jobId: 'job-b' }),
      ev.jobStatus('job-b', 'answered'),
      ev.kwRunClosed('run-b', 'finished'),
      ev.runFinished('run-b'),
    ]);
    expect(katStatus()).toHaveTextContent(providerDown);
    expect(katStatus()).not.toHaveTextContent('นักเขียน');

    // A Writer run that stops on time_limit fills only the Writer's slot.
    seq = stream(source, seq, [
      ev.kwRunStarted('run-c', WRITER, '05-reply', { jobId: 'job-c' }),
      ev.jobStatus('job-c', 'stopped', 'time_limit'),
      ev.kwRunClosed('run-c', 'finished', 'time_limit'),
    ]);
    expect(katStatus()).toHaveTextContent(providerDown);
    expect(katStatus()).toHaveTextContent(writerTimeout);

    // The Lead's later answered job clears only the Lead's slot.
    seq = stream(source, seq, [
      ev.kwRunStarted('run-d', LEAD, '06-reply', { jobId: 'job-d' }),
      ev.stored('06-reply', 'ได้แล้ว', { author: { kind: 'kat', id: LEAD }, jobId: 'job-d' }),
      ev.jobStatus('job-d', 'answered'),
      ev.kwRunClosed('run-d', 'finished'),
    ]);
    expect(katStatus()).not.toHaveTextContent(providerDown);
    expect(katStatus()).toHaveTextContent(writerTimeout);

    // A cancelled close and a question close fill no slot.
    stream(source, seq, [
      ev.kwRunStarted('run-e', LEAD, '07-reply', { jobId: 'job-e' }),
      ev.kwRunClosed('run-e', 'cancelled', 'cancelled'),
      ev.runError('cancelled'),
      ev.kwRunStarted('run-f', LEAD, '08-reply', { jobId: 'job-f' }),
      ev.kwRunClosed('run-f', 'finished', 'question_pending'),
    ]);
    expect(katStatus().textContent).toBe(writerTimeout);
  });

  test('pause_stop_shows_the_stopped_copy_never_the_waiting_copy', async () => {
    api();
    const { source } = await openThread();
    stream(source, 1, [
      ev.kwRunStarted('run-1', LEAD, REPLY, { jobId: 'job-1' }),
      ev.notice(NOTICE, 'workspace_paused_stopped', [{ id: LEAD, name: 'หัวหน้าทีม' }]),
      // The job keeps the stored reason; the run closes with it too.
      ev.jobStatus('job-1', 'stopped', 'workspace_paused'),
      ev.kwRunClosed('run-1', 'finished', 'workspace_paused'),
    ]);
    expect(katStatus()).toHaveTextContent(en['reason.workspace_paused_stopped']);
    expect(screen.queryByText(en['reason.workspace_paused'])).toBeNull();
    expect(
      screen.getByText(en['reason.workspace_paused_stopped'], { selector: '.message-body' }),
    ).toBeVisible();
  });

  test('chat_shows_queue_position_while_the_kat_is_busy', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const calls = api({
      [`GET ${BASE}/jobs/job-1`]: [
        200,
        { id: 'job-1', status: null, reason: null, hold_reason: 'kat_busy', queue_position: 1 },
      ],
      [`GET ${BASE}/jobs/job-2`]: [
        [
          200,
          { id: 'job-2', status: null, reason: null, hold_reason: 'kat_busy', queue_position: 2 },
        ],
        [
          200,
          { id: 'job-2', status: null, reason: null, hold_reason: 'kat_busy', queue_position: 1 },
        ],
      ],
    });
    const { source } = await openThread();
    const seq = stream(source, 1, [
      ev.userStored(ASK, 'First', { userId: ME, jobId: 'job-1' }),
      ev.jobStatus('job-1', null),
      ev.userStored(SECOND, 'Second', { userId: ME, jobId: 'job-2' }),
      ev.jobStatus('job-2', null),
      ev.jobStatus('job-1', null, null, 'kat_busy'),
      ev.jobStatus('job-2', null, null, 'kat_busy'),
    ]);
    const line = (position: number) =>
      en['chat.queue_position'].replace('{position}', String(position));
    expect(await screen.findByText(line(1))).toBeVisible();
    expect(await screen.findByText(line(2))).toBeVisible();
    act(() => setLocale('th'));
    expect(screen.getByText(th['chat.queue_position'].replace('{position}', '2'))).toBeVisible();
    act(() => setLocale('en'));

    // The first job's run starts: its line goes, the other job's stays and refreshes.
    stream(source, seq, [ev.kwRunStarted('run-1', LEAD, REPLY, { jobId: 'job-1' })]);
    expect(screen.queryByText(line(1))).toBeNull();
    expect(screen.getByText(line(2))).toBeVisible();
    await act(() => vi.advanceTimersByTimeAsync(5_000));
    expect(calls.callsTo(`GET ${BASE}/jobs/job-2`)).toHaveLength(2);
    expect(await screen.findByText(line(1))).toBeVisible();
    expect(calls.callsTo(`GET ${BASE}/jobs/job-1`)).toHaveLength(1);
  });

  test('transcript_log_excludes_provisional_text', async () => {
    api();
    const { source } = await openThread();
    stream(source, 1, [
      ev.userStored(ASK, 'Hello', { userId: ME }),
      ev.kwRunStarted('run-1', LEAD, REPLY),
      ev.textStart(REPLY),
      ev.text(REPLY, 'Streaming words'),
    ]);
    const streaming = screen.getByText('Streaming words').closest('li')!;
    expect(streaming).toHaveAttribute('aria-hidden', 'true');
    expect(announced()).toEqual([`${en['chat.author.you']}: Hello`]);
    stream(source, 5, [ev.stored(REPLY, 'Stored words', { author: { kind: 'kat', id: LEAD } })]);
    expect(announced()).toEqual([`${en['chat.author.you']}: Hello`, 'หัวหน้าทีม: Stored words']);
  });

  test('typing_status_role_status', async () => {
    api();
    const { source } = await openThread();
    const status = katStatus();
    expect(status).toHaveAttribute('role', 'status');
    stream(source, 1, [
      ev.kwRunStarted('run-1', LEAD, REPLY),
      ev.katState(LEAD, 'working'),
      ev.kwRunStarted('run-2', WRITER, SECOND),
      ev.katState(WRITER, 'researching'),
    ]);
    expect(status).toHaveTextContent(en['chat.replying'].replace('{kat}', 'หัวหน้าทีม'));
    expect(status).toHaveTextContent(en['chat.replying'].replace('{kat}', 'นักเขียน'));
    act(() => setLocale('th'));
    expect(status).toHaveTextContent(th['chat.replying'].replace('{kat}', 'หัวหน้าทีม'));
    act(() => setLocale('en'));
    stream(source, 5, [ev.katState(LEAD, 'idle'), ev.kwRunClosed('run-1', 'finished')]);
    expect(status).not.toHaveTextContent('หัวหน้าทีม');
  });

  test('replying_stays_while_any_run_of_the_kat_streams', async () => {
    // The Lead runs up to three jobs at once; its kat_state goes idle when the first ends.
    api();
    const { source } = await openThread();
    const replying = en['chat.replying'].replace('{kat}', 'หัวหน้าทีม');
    stream(source, 1, [
      ev.kwRunStarted('run-1', LEAD, REPLY),
      ev.kwRunStarted('run-2', LEAD, SECOND),
      ev.katState(LEAD, 'working'),
    ]);
    expect(katStatus()).toHaveTextContent(replying);
    stream(source, 4, [
      ev.stored(REPLY, 'First answer', { author: { kind: 'kat', id: LEAD } }),
      ev.katState(LEAD, 'idle'),
      ev.kwRunClosed('run-1', 'finished'),
    ]);
    expect(katStatus()).toHaveTextContent(replying);
    expect(katStatus().textContent!.split(replying)).toHaveLength(2);
    stream(source, 7, [ev.kwRunClosed('run-2', 'finished')]);
    expect(katStatus()).not.toHaveTextContent(replying);
  });
});

test('composer_offers_the_team_for_mentions', async () => {
  api();
  const user = userEvent.setup({ delay: null });
  await openThread();
  await user.type(screen.getByRole('textbox', { name: en['chat.composer_label'] }), '@');
  expect(
    within(screen.getByRole('listbox'))
      .getAllByRole('option')
      .map((o) => o.textContent),
  ).toEqual(THAI_TEAM.members.map((member) => member.name));
});

describe('history', () => {
  const id = (n: number) => `0000-${String(n).padStart(4, '0')}`;
  const messages = (from: number, to: number) =>
    Array.from({ length: to - from + 1 }, (_, i) =>
      apiMessage(id(from + i), `message ${from + i}`, { author: { kind: 'user', id: ME } }),
    );

  test('show_earlier_messages_loads_older_pages_and_keeps_the_scroll_position', async () => {
    const calls = api();
    calls.route(MESSAGES, ({ path }) => {
      const before = new URL(path, 'http://localhost').searchParams.get('before');
      if (before === null) return page(messages(51, 100), 3);
      if (before === id(51)) return page(messages(1, 50), 3);
      return page([], 3);
    });
    const user = userEvent.setup({ delay: null });
    await openThread();
    expect(bodies()).toHaveLength(50);

    // jsdom has no layout: give the scroller a height that grows with its messages.
    const scroller = list().closest('.transcript-content')!.parentElement!;
    let top = 120;
    Object.defineProperty(scroller, 'scrollHeight', {
      configurable: true,
      get: () => scroller.querySelectorAll('.message').length * 20,
    });
    Object.defineProperty(scroller, 'scrollTop', {
      configurable: true,
      get: () => top,
      set: (value: number) => {
        top = value;
      },
    });

    await user.click(screen.getByRole('button', { name: en['chat.earlier'] }));
    await waitFor(() => expect(bodies()).toHaveLength(100));
    expect(calls.callsTo(MESSAGES).at(-1)!.path).toContain(`before=${id(51)}`);
    expect(bodies()[0]).toBe('message 1');
    // One list in id order, outside any live region: a screen reader is not read 50 old
    // messages; one line says what arrived.
    expect(within(list()).getAllByRole('listitem')).toHaveLength(100);
    expect(list().closest('[aria-live]')).toBeNull();
    expect(announced()).toEqual([]);
    expect(
      screen.getByText(en['chat.earlier_loaded.other'].replace('{count}', '50')),
    ).toBeInTheDocument();
    act(() => setLocale('th'));
    expect(
      screen.getByText(th['chat.earlier_loaded.other'].replace('{count}', '50')),
    ).toBeInTheDocument();
    act(() => setLocale('en'));
    // The message that was in view stays in view: 880 px from the bottom, before and after.
    expect(top).toBe(100 * 20 - (50 * 20 - 120));

    await user.click(screen.getByRole('button', { name: en['chat.earlier'] }));
    const start = await screen.findByText(en['chat.start']);
    expect(screen.queryByRole('button', { name: en['chat.earlier'] })).toBeNull();
    expect(start).toHaveFocus();
    expect(bodies()).toHaveLength(100);
  });

  test('a_late_older_reply_never_moves_the_history_cursor_or_overwrites_newer_state', async () => {
    // A slow run's reply id was minted before the first page's oldest message; its stored event
    // lands while the person reads, and the older page then holds a stale copy of it.
    const stale = messages(1, 50).map((message) => {
      if (message.id === id(24)) return { ...message, job_id: 'job-old' };
      if (message.id === id(25)) {
        return apiMessage(id(25), 'Draft answer', {
          author: { kind: 'kat', id: LEAD },
          job_id: 'job-old',
        });
      }
      return message;
    });
    const calls = api();
    calls.route(MESSAGES, ({ path }) => {
      const before = new URL(path, 'http://localhost').searchParams.get('before');
      if (before === null) return page(messages(51, 100), 3);
      if (before === id(51)) return page(stale, 3);
      return page([], 3);
    });
    const user = userEvent.setup({ delay: null });
    const { source } = await openThread();
    stream(source, 4, [
      ev.stored(id(25), 'Final answer', { author: { kind: 'kat', id: LEAD }, jobId: 'job-old' }),
      ev.jobStatus('job-old', 'answered'),
    ]);
    expect(bodies()).toHaveLength(51);

    await user.click(screen.getByRole('button', { name: en['chat.earlier'] }));
    await waitFor(() => expect(bodies()).toHaveLength(100));
    // The cursor came from the REST page, not from the live reply: nothing in 26-50 is skipped.
    expect(calls.callsTo(MESSAGES).at(-1)!.path).toContain(`before=${id(51)}`);
    expect(bodies().slice(23, 27)).toEqual([
      'message 24',
      'Final answer',
      'message 26',
      'message 27',
    ]);
    // The older page never overwrites what the stream said since: no stale body, no Stop.
    expect(screen.queryByText('Draft answer')).toBeNull();
    expect(screen.queryByRole('button', { name: en['chat.stop'] })).toBeNull();
  });

  // A slow run's reply 25 was minted before the first page (51-100) and stored late, so the
  // REST page of older messages may or may not hold it.
  const withoutReply = () => messages(1, 50).filter((message) => message.id !== id(25));
  const lateReply = () =>
    ev.stored(id(25), 'Late reply', {
      author: { kind: 'kat', id: LEAD },
      authorName: 'หัวหน้าทีม',
    });

  test('a_reply_stored_after_an_older_page_keeps_message_id_order', async () => {
    const calls = api();
    calls.route(MESSAGES, ({ path }) => {
      const before = new URL(path, 'http://localhost').searchParams.get('before');
      return before === null ? page(messages(51, 100), 3) : page(withoutReply(), 3);
    });
    const user = userEvent.setup({ delay: null });
    const { source } = await openThread();
    await user.click(screen.getByRole('button', { name: en['chat.earlier'] }));
    await waitFor(() => expect(bodies()).toHaveLength(99));
    expect(announced()).toEqual([]);

    stream(source, 4, [lateReply()]);
    expect(bodies().slice(23, 26)).toEqual(['message 24', 'Late reply', 'message 26']);
    expect(bodies()).toHaveLength(100);
    // Only the new arrival is announced, never the history around it.
    expect(announced()).toEqual(['หัวหน้าทีม: Late reply']);
  });

  test('a_reply_stored_during_an_older_page_request_keeps_message_id_order', async () => {
    const held = deferred<Reply>();
    const calls = api();
    calls.route(MESSAGES, ({ path }) => {
      const before = new URL(path, 'http://localhost').searchParams.get('before');
      return before === null ? page(messages(51, 100), 3) : held.promise;
    });
    const user = userEvent.setup({ delay: null });
    const { source } = await openThread();
    await user.click(screen.getByRole('button', { name: en['chat.earlier'] }));
    // The page was read before the reply was stored, so it lacks it.
    stream(source, 4, [lateReply()]);
    expect(announced()).toEqual(['หัวหน้าทีม: Late reply']);
    await act(async () => held.resolve(page(withoutReply(), 3)));
    await waitFor(() => expect(bodies()).toHaveLength(100));
    expect(bodies().slice(23, 26)).toEqual(['message 24', 'Late reply', 'message 26']);
    expect(announced()).toEqual(['หัวหน้าทีม: Late reply']);
  });

  test('every_full_older_page_is_announced', async () => {
    const held = deferred<Reply>();
    const calls = api();
    calls.route(MESSAGES, ({ path }) => {
      const before = new URL(path, 'http://localhost').searchParams.get('before');
      if (before === null) return page(messages(101, 150), 3);
      if (before === id(101)) return page(messages(51, 100), 3);
      return held.promise;
    });
    const user = userEvent.setup({ delay: null });
    await openThread();
    const loaded = en['chat.earlier_loaded.other'].replace('{count}', '50');
    const status = () => screen.getByTestId('earlier-status');
    await user.click(screen.getByRole('button', { name: en['chat.earlier'] }));
    await waitFor(() => expect(status()).toHaveTextContent(loaded));

    // The same count again must still change the status, or a screen reader stays silent.
    await user.click(screen.getByRole('button', { name: en['chat.earlier'] }));
    expect(status()).toHaveTextContent('');
    await act(async () => held.resolve(page(messages(1, 50), 3)));
    await waitFor(() => expect(status()).toHaveTextContent(loaded));
    expect(bodies()).toHaveLength(150);
  });

  test('a_single_earlier_message_is_announced_in_the_singular', async () => {
    const calls = api();
    calls.route(MESSAGES, ({ path }) => {
      const before = new URL(path, 'http://localhost').searchParams.get('before');
      return before === null ? page(messages(2, 51), 3) : page(messages(1, 1), 3);
    });
    const user = userEvent.setup({ delay: null });
    await openThread();
    await user.click(screen.getByRole('button', { name: en['chat.earlier'] }));
    expect(await screen.findByText('1 earlier message loaded')).toBeInTheDocument();
    act(() => setLocale('th'));
    expect(screen.getByText('โหลดข้อความก่อนหน้าแล้ว 1 ข้อความ')).toBeInTheDocument();
  });

  test('a_short_conversation_offers_no_earlier_messages', async () => {
    api({ [MESSAGES]: page(messages(1, 12), 3) });
    await openThread();
    expect(bodies()).toHaveLength(12);
    expect(screen.queryByRole('button', { name: en['chat.earlier'] })).toBeNull();
  });
});

test('untitled_conversations_are_told_apart_by_their_start_time', async () => {
  const earlier = {
    id: '0192b3c4-d5e6-7000-8000-000000000001',
    space_id: 'space-1',
    team_id: TEAM,
    title: '',
  };
  const later = {
    id: '0192b4c4-d5e6-7000-8000-000000000002',
    space_id: 'space-1',
    team_id: TEAM,
    title: '',
  };
  api({ [`GET ${BASE}/spaces/space-1/threads`]: [200, [later, earlier]] });
  renderChat({ threadId: null });
  const list = await screen.findByRole('list', { name: en['chat.title'] });
  const labels = within(list)
    .getAllByRole('link')
    .map((link) => link.textContent);
  expect(labels).toHaveLength(2);
  expect(new Set(labels).size).toBe(2);
  for (const label of labels) expect(label).toMatch(/^Untitled conversation from /);
});

test('a_poll_page_read_before_a_folded_event_is_dropped', () => {
  // The poll can resolve in the same tick as the first live event, before the polling effect is
  // cleaned up; the reducer's seq check alone drops it.
  const asked = apiMessage(ASK, 'Write it', { author: { kind: 'user', id: ME }, job_id: 'job-1' });
  const snapshot = reduceChat(emptyTranscript(), {
    type: 'snapshot',
    page: { messages: [asked], event_seq: 2 },
  });
  const live = reduceChat(snapshot, {
    type: 'event',
    event: { seq: 3, event: ev.jobStatus('job-1', 'answered') },
  });
  const late = reduceChat(live, { type: 'merge', messages: [asked], issuedAt: 2 });
  expect(late).toBe(live);
  expect(runningJobs(late)).toEqual([]);
  // A page read after that event is applied.
  const fresh = reduceChat(live, {
    type: 'merge',
    messages: [
      { ...asked, job_status: 'answered' },
      apiMessage(REPLY, 'Done', { job_id: 'job-1' }),
    ],
    issuedAt: 3,
  });
  expect(fresh.messages[REPLY]?.body).toBe('Done');
});
