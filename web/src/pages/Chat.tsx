import { useEffect, useReducer, useRef, useState, type FormEvent } from 'react';
import { api, ApiError, errorMessage, MESSAGE_PAGE, type MessagePage, type Thread } from '../api';
import { newClientMsgId } from '../client-msg-id';
import { Composer } from '../components/Composer';
import { PageTitle } from '../components/Layout';
import { Transcript, type Earlier, type PendingMessage } from '../components/Transcript';
import { formatDateTime, hasMessage, t, useLocale } from '../i18n';
import { Link, navigate } from '../router';
import {
  emptyTranscript,
  foldEvent,
  mergeEarlier,
  mergeMessages,
  putPosted,
  replyingKats,
  runningJobs,
  subscribe,
  transcriptFromSnapshot,
  transcriptMessages,
  type ApiMessage,
  type SeqEvent,
  type StreamStatus,
  type Transcript as TranscriptState,
} from '../sse';
import { useTeam } from './Team';

/** A hidden page gives its stream back after this long (the browser's six-connection budget). */
const HIDDEN_CLOSE_MS = 60_000;
/** How often the page re-reads the messages while it is over the stream budget, or a job waits. */
const POLL_MS = 5_000;

// A run that a pause stopped closes with the job's stored reason, whose copy says the work
// waits. It did not wait, so it is shown with the stopped notice's copy.
const STOPPED_COPY = new Map([
  ['workspace_paused', 'workspace_paused_stopped'],
  ['kat_paused', 'kat_paused_stopped'],
]);

function runEndCopy(code: string): string {
  const key = `reason.${STOPPED_COPY.get(code) ?? code}`;
  return t(hasMessage(key) ? key : 'reason.unknown');
}

const isUnauthenticated = (error: unknown) => error instanceof ApiError && error.status === 401;

/** The creation time a UUIDv7 id carries in its first 48 bits, or null for any other id. */
export function uuidv7Time(id: string): Date | null {
  const hex = id.replace(/-/g, '');
  if (!/^[0-9a-f]{32}$/i.test(hex) || hex[12] !== '7') return null;
  return new Date(parseInt(hex.slice(0, 12), 16));
}

/** A thread's title, or for an untitled one the time it started, so several can be told apart. */
export function threadLabel(thread: Thread): string {
  if (thread.title) return thread.title;
  const started = uuidv7Time(thread.id);
  return started ? t('chat.untitled_at', { time: formatDateTime(started) }) : t('chat.untitled');
}

interface Props {
  workspaceId: string;
  spaceId: string;
  /** The Space's default team, for new conversations. */
  teamId: string | null;
  threadId: string | null;
  /** The signed-in user. */
  me: string;
  onSignedOut: () => void;
}

/** `/chat/:threadId?`: the Space's conversations, or one conversation. */
export function Chat({ workspaceId, spaceId, teamId, threadId, me, onSignedOut }: Props) {
  useLocale();
  if (threadId) {
    return (
      <Conversation
        key={`${workspaceId}/${threadId}`}
        workspaceId={workspaceId}
        threadId={threadId}
        me={me}
        onSignedOut={onSignedOut}
      />
    );
  }
  return (
    <div className="page">
      <div className="page-intro">
        <PageTitle>{t('chat.title')}</PageTitle>
        <p className="lede">{t('chat.lede')}</p>
      </div>
      <Conversations workspaceId={workspaceId} spaceId={spaceId} teamId={teamId} />
    </div>
  );
}

/** The Space's threads, newest first, and a form for a new one. */
export function Conversations({
  workspaceId,
  spaceId,
  teamId,
  limit,
  compact = false,
}: {
  workspaceId: string;
  spaceId: string;
  teamId: string | null;
  limit?: number;
  /** Inside a panel that has its own heading. */
  compact?: boolean;
}) {
  useLocale();
  const [threads, setThreads] = useState<Thread[] | null>(null);
  const [failure, setFailure] = useState<unknown>(null);
  useEffect(() => {
    let live = true;
    setThreads(null);
    api.threads(workspaceId, spaceId).then(
      (list) => live && setThreads(list),
      (error: unknown) => live && setFailure(error),
    );
    return () => {
      live = false;
    };
  }, [workspaceId, spaceId]);

  // The server lists them newest first; ids are UUIDv7, so the same order holds by id.
  const shown = threads
    ? [...threads].sort((a, b) => (a.id < b.id ? 1 : a.id > b.id ? -1 : 0)).slice(0, limit)
    : null;
  return (
    <div className="conversations">
      <NewConversation
        workspaceId={workspaceId}
        spaceId={spaceId}
        teamId={teamId}
        compact={compact}
      />
      {failure !== null && (
        <div className="alert" role="alert">
          <p>{errorMessage(failure)}</p>
        </div>
      )}
      {shown === null ? (
        failure === null && <p className="hint">{t('chat.loading')}</p>
      ) : shown.length === 0 ? (
        <p>{t('chat.no_threads')}</p>
      ) : (
        <ul className="thread-list" aria-label={t('chat.title')}>
          {shown.map((thread) => (
            <li key={thread.id}>
              <Link to={`/chat/${thread.id}`} className="thread-link">
                {threadLabel(thread)}
              </Link>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function NewConversation({
  workspaceId,
  spaceId,
  teamId,
  compact,
}: {
  workspaceId: string;
  spaceId: string;
  teamId: string | null;
  compact: boolean;
}) {
  useLocale();
  const [title, setTitle] = useState('');
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<unknown>(null);
  const id = `new-thread-${spaceId}`;

  const start = async (event: FormEvent) => {
    event.preventDefault();
    if (!teamId || busy) return;
    setBusy(true);
    setFailure(null);
    try {
      const thread = await api.createThread(workspaceId, {
        space_id: spaceId,
        team_id: teamId,
        title,
      });
      navigate(`/chat/${thread.id}`);
    } catch (error) {
      setFailure(error);
      setBusy(false);
    }
  };

  return (
    <form className="new-thread" onSubmit={start} aria-labelledby={`${id}-heading`}>
      {compact ? (
        <h3 id={`${id}-heading`}>{t('chat.new')}</h3>
      ) : (
        <h2 id={`${id}-heading`}>{t('chat.new')}</h2>
      )}
      <div className="new-thread-row">
        <div className="field">
          <label htmlFor={`${id}-title`}>{t('chat.new_title')}</label>
          <input
            id={`${id}-title`}
            value={title}
            maxLength={120}
            aria-invalid={failure !== null || undefined}
            aria-describedby={failure !== null ? `${id}-error` : undefined}
            onChange={(event) => setTitle(event.target.value)}
          />
        </div>
        <button type="submit" className="button button-primary" disabled={!teamId || busy}>
          {busy ? t('chat.creating') : t('chat.create')}
        </button>
      </div>
      {!teamId && <p className="hint">{t('team.none')}</p>}
      {failure !== null && (
        <p id={`${id}-error`} className="field-error" role="alert">
          {errorMessage(failure)}
        </p>
      )}
    </form>
  );
}

type Action =
  | { type: 'snapshot'; page: MessagePage }
  | { type: 'event'; event: SeqEvent }
  /** A polled page, read when the transcript was at seq `issuedAt`. */
  | { type: 'merge'; messages: ApiMessage[]; issuedAt: number }
  | { type: 'earlier'; messages: ApiMessage[] }
  | { type: 'posted'; posted: Parameters<typeof putPosted>[1] };

export function reduceChat(state: TranscriptState, action: Action): TranscriptState {
  switch (action.type) {
    case 'snapshot':
      return transcriptFromSnapshot(action.page.messages, action.page.event_seq);
    case 'event':
      return foldEvent(state, action.event);
    case 'merge':
      // An event folded since the page was read is newer than the page.
      return state.lastSeq > action.issuedAt ? state : mergeMessages(state, action.messages);
    case 'earlier':
      return mergeEarlier(state, action.messages);
    case 'posted':
      return putPosted(state, action.posted);
  }
}

interface Post {
  clientMsgId: string;
  body: string;
  failure: unknown;
}

function Conversation({
  workspaceId,
  threadId,
  me,
  onSignedOut,
}: {
  workspaceId: string;
  threadId: string;
  me: string;
  onSignedOut: () => void;
}) {
  useLocale();
  const [thread, setThread] = useState<Thread | null>(null);
  const [failure, setFailure] = useState<unknown>(null);
  const [transcript, dispatch] = useReducer(reduceChat, undefined, emptyTranscript);
  // Bumped when a snapshot lands; the stream opens from its `event_seq`.
  const [snapshots, setSnapshots] = useState(0);
  // Bumped to load the snapshot again (`resync_required`).
  const [reloads, setReloads] = useState(0);
  const [status, setStatus] = useState<StreamStatus | null>(null);
  // False once the page has been hidden for a minute.
  const [streaming, setStreaming] = useState(true);
  const [posts, setPosts] = useState<Post[]>([]);
  const [finished, setFinished] = useState<ReadonlySet<string>>(new Set());
  const [stopFailure, setStopFailure] = useState<unknown>(null);
  const [answered, setAnswered] = useState<string | null>(null);
  const [earlier, setEarlier] = useState<Earlier>('none');
  // Where the next older page starts: the oldest id of the last REST history page. Only those
  // pages move it; a live reply minted long ago (an id older than the first page) must not, or
  // the history between them would never load.
  const historyCursor = useRef<string | null>(null);
  // Ids from history, the snapshot and older pages: listed in id order like the rest, but never
  // announced as new; `earlierLoaded` says how many each older page brought instead.
  const [history, setHistory] = useState<ReadonlySet<string>>(new Set());
  const [earlierLoaded, setEarlierLoaded] = useState<number | null>(null);
  const composerInput = useRef<HTMLTextAreaElement>(null);
  const cancelling = useRef(new Set<string>());
  const [, setCancels] = useState(0);
  const lastSeq = useRef(0);
  lastSeq.current = transcript.lastSeq;
  const signedOut = useRef(onSignedOut);
  signedOut.current = onSignedOut;
  const team = useTeam(workspaceId, thread?.team_id ?? null);

  useEffect(() => {
    let live = true;
    Promise.all([api.thread(workspaceId, threadId), api.messages(workspaceId, threadId)]).then(
      ([loaded, page]) => {
        if (!live) return;
        setThread(loaded);
        dispatch({ type: 'snapshot', page });
        setEarlier(page.messages.length >= MESSAGE_PAGE ? 'more' : 'none');
        historyCursor.current = page.messages[0]?.id ?? null;
        setHistory(new Set(page.messages.map((message) => message.id)));
        setEarlierLoaded(null);
        setSnapshots((n) => n + 1);
      },
      (error: unknown) => {
        if (!live) return;
        if (isUnauthenticated(error)) signedOut.current();
        else setFailure(error);
      },
    );
    return () => {
      live = false;
    };
  }, [workspaceId, threadId, reloads]);

  // One stream per open page: opened from the snapshot, closed on unmount or a minute hidden.
  useEffect(() => {
    if (snapshots === 0) return;
    // Hidden for a minute: no stream, and so no polling either, until the page is visible.
    setStatus(null);
    if (!streaming) return;
    return subscribe(
      api.eventsPath(workspaceId, threadId),
      (event) => dispatch({ type: 'event', event }),
      {
        after: lastSeq.current,
        onStatus: setStatus,
      },
    );
  }, [workspaceId, threadId, snapshots, streaming]);

  useEffect(() => {
    if (status === 'resync') setReloads((n) => n + 1);
    if (status === 'unauthenticated') signedOut.current();
  }, [status]);

  // Over the session's stream budget: stored messages, job statuses and Stop stay current. A
  // page answered after polling ended, or after a later poll's page, is dropped.
  useEffect(() => {
    if (status !== 'polling') return;
    let live = true;
    let sent = 0;
    let applied = 0;
    const timer = setInterval(() => {
      const poll = (sent += 1);
      const issuedAt = lastSeq.current;
      api.messages(workspaceId, threadId).then(
        (page) => {
          if (!live || poll < applied) return;
          applied = poll;
          dispatch({ type: 'merge', messages: page.messages, issuedAt });
        },
        () => {},
      );
    }, POLL_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [status, workspaceId, threadId]);

  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const onChange = () => {
      clearTimeout(timer);
      if (document.visibilityState === 'hidden')
        timer = setTimeout(() => setStreaming(false), HIDDEN_CLOSE_MS);
      else setStreaming(true);
    };
    // A chat opened in a background tab gets no visibility change until it is shown.
    onChange();
    document.addEventListener('visibilitychange', onChange);
    return () => {
      clearTimeout(timer);
      document.removeEventListener('visibilitychange', onChange);
    };
  }, []);

  if (failure !== null) {
    return (
      <div className="page">
        <div className="alert" role="alert">
          <p>{errorMessage(failure)}</p>
        </div>
        <p>
          <Link to="/chat" className="link">
            {t('chat.back')}
          </Link>
        </p>
      </div>
    );
  }
  if (!thread || snapshots === 0) {
    return (
      <p className="hint" aria-busy="true">
        {t('chat.loading')}
      </p>
    );
  }

  const members = team.state === 'ready' ? team.view.members : [];
  const katName = (katId: string) => members.find((member) => member.id === katId)?.name;
  const nameOf = (katId: string) => katName(katId) ?? t('chat.author.kat');

  const deliver = (post: Post) => {
    api.postMessage(workspaceId, threadId, post.body, post.clientMsgId).then(
      (posted) => {
        dispatch({
          type: 'posted',
          posted: {
            id: posted.message_id,
            author: { kind: 'user', id: me },
            body: post.body,
            clientMsgId: post.clientMsgId,
            jobId: posted.job_id,
          },
        });
        setPosts((list) => list.filter((other) => other.clientMsgId !== post.clientMsgId));
      },
      (error: unknown) => {
        if (isUnauthenticated(error)) signedOut.current();
        setPosts((list) =>
          list.map((other) =>
            other.clientMsgId === post.clientMsgId ? { ...other, failure: error } : other,
          ),
        );
      },
    );
  };
  // A fresh UUID v4 per composed message, reused by every retry of that message.
  const send = (body: string) => {
    const post = { clientMsgId: newClientMsgId(), body, failure: null };
    setPosts((list) => [...list, post]);
    deliver(post);
  };
  const retry = (clientMsgId: string) => {
    const post = posts.find((other) => other.clientMsgId === clientMsgId);
    if (!post) return;
    setPosts((list) =>
      list.map((other) =>
        other.clientMsgId === clientMsgId ? { ...other, failure: null } : other,
      ),
    );
    deliver({ ...post, failure: null });
  };

  const messages = transcriptMessages(transcript);
  // A post the stream already confirmed shows as the stored message, even before its response.
  const confirmed = new Set(
    messages
      .filter((message) => message.author?.kind === 'user' && message.author.id === me)
      .map((message) => message.clientMsgId),
  );
  const pending: PendingMessage[] = posts
    .filter((post) => !confirmed.has(post.clientMsgId))
    .map((post) => ({
      clientMsgId: post.clientMsgId,
      body: post.body,
      error: post.failure === null ? null : errorMessage(post.failure),
    }));

  // A question can be answered only while it is the latest message and its job waits on it.
  const latest = messages.filter((message) => !message.provisional).at(-1);
  const latestJob = latest?.jobId ? transcript.jobs[latest.jobId] : undefined;
  const questionOpen =
    latest !== undefined &&
    (latest.question?.options.length ?? 0) > 0 &&
    latestJob?.status === 'needs_input' &&
    latestJob.reason === 'question_pending';
  const activeQuestion =
    questionOpen && (pending.length === 0 || answered === latest.id) ? latest.id : null;
  const answer = (option: string) => {
    if (!activeQuestion || answered === activeQuestion) return;
    setAnswered(activeQuestion);
    send(option);
    // The options turn disabled; the composer is where the conversation goes on.
    composerInput.current?.focus();
  };

  const loadEarlier = async () => {
    const before = historyCursor.current;
    if (!before || earlier !== 'more') return;
    setEarlier('loading');
    // Cleared now and set when the page lands, so a second page of the same size still changes
    // the status line and is announced.
    setEarlierLoaded(null);
    try {
      const page = await api.messages(workspaceId, threadId, before);
      dispatch({ type: 'earlier', messages: page.messages });
      historyCursor.current = page.messages[0]?.id ?? before;
      setHistory((ids) => new Set([...ids, ...page.messages.map((message) => message.id)]));
      setEarlierLoaded(page.messages.length);
      setEarlier(page.messages.length >= MESSAGE_PAGE ? 'more' : 'start');
    } catch (error) {
      if (isUnauthenticated(error)) signedOut.current();
      setEarlier('more');
    }
  };

  // Stop cancels only this person's requests (coordinator ruling for PR #46); a colleague's work
  // in a shared thread shows as work in progress. Task 12 enforces who may cancel.
  const requester = new Map<string, string>();
  for (const message of messages) {
    if (message.author?.kind === 'user' && message.jobId)
      requester.set(message.jobId, message.author.id);
  }
  const unfinished = runningJobs(transcript).filter((job) => !finished.has(job));
  const running = unfinished.filter((job) => requester.get(job) === me);
  const othersWorking = unfinished.some((job) => {
    const by = requester.get(job);
    return by !== undefined && by !== me;
  });
  const stopping = running.length > 0 && running.every((job) => cancelling.current.has(job));
  const stop = () => {
    setStopFailure(null);
    for (const job of running) {
      if (cancelling.current.has(job)) continue;
      cancelling.current.add(job);
      api.cancelJob(workspaceId, job).then(
        () => {},
        (error: unknown) => {
          cancelling.current.delete(job);
          if (error instanceof ApiError && error.code === 'job_already_finished') {
            setFinished((done) => new Set(done).add(job));
          } else {
            setStopFailure(error);
          }
          setCancels((n) => n + 1);
        },
      );
    }
    setCancels((n) => n + 1);
  };

  // A Kat replies while any of its runs is open: the Lead runs several at once.
  const replying = replyingKats(transcript);
  const queued = Object.entries(transcript.jobs)
    .filter(([, job]) => job.status === null && job.holdReason === 'kat_busy' && !job.started)
    .map(([jobId]) => jobId);

  return (
    <div className="chat">
      <div className="chat-header">
        <p>
          <Link to="/chat" className="link">
            {t('chat.back')}
          </Link>
        </p>
        <PageTitle>{threadLabel(thread)}</PageTitle>
        {team.state === 'ready' && (
          <p className="chat-team">
            <Link to={`/team/${thread.team_id}`} className="link">
              {team.view.team.name}
            </Link>
          </p>
        )}
      </div>
      <Transcript
        messages={messages}
        pending={pending}
        me={me}
        katName={katName}
        activeQuestion={activeQuestion}
        optionsDisabled={answered !== null && answered === activeQuestion}
        onOption={answer}
        onRetry={retry}
        earlier={earlier}
        history={history}
        earlierLoaded={earlierLoaded}
        onEarlier={loadEarlier}
      />
      <div className="chat-status">
        <div className="kat-status" role="status" data-testid="kat-status">
          {replying.map((katId) => (
            <p key={`replying-${katId}`} className="kat-status-line">
              {t('chat.replying', { kat: nameOf(katId) })}
            </p>
          ))}
          {othersWorking && <p className="kat-status-line">{t('chat.others_working')}</p>}
          {Object.entries(transcript.errors).map(([katId, code]) => (
            <p key={`error-${katId}`} className="kat-status-line kat-status-error">
              {t('chat.kat_error', { kat: nameOf(katId), message: runEndCopy(code) })}
            </p>
          ))}
        </div>
        <div className="queue-status" role="status">
          {queued.map((jobId) => (
            <QueuePosition key={jobId} workspaceId={workspaceId} jobId={jobId} />
          ))}
        </div>
        <p className="stream-status" role="status">
          {status === 'reconnecting' && t('chat.reconnecting')}
          {status === 'polling' && t('chat.polling')}
        </p>
      </div>
      {stopFailure !== null && (
        <div className="alert" role="alert">
          <p>{t('chat.stop_failed')}</p>
          <p>{errorMessage(stopFailure)}</p>
        </div>
      )}
      <div className="chat-compose">
        {running.length > 0 && (
          <button type="button" className="button button-stop" disabled={stopping} onClick={stop}>
            {stopping ? t('chat.stopping') : t('chat.stop')}
          </button>
        )}
        <Composer
          onSend={send}
          inputRef={composerInput}
          mentions={members.map(({ id, name }) => ({ id, name }))}
        />
      </div>
    </div>
  );
}

/** `Waiting in line: N` for a job held while its Kat is busy, refreshed every 5 s. */
function QueuePosition({ workspaceId, jobId }: { workspaceId: string; jobId: string }) {
  useLocale();
  const [position, setPosition] = useState<number | null>(null);
  useEffect(() => {
    let live = true;
    const load = () =>
      api.job(workspaceId, jobId).then(
        (view) => live && setPosition(view.queue_position),
        () => {},
      );
    void load();
    const timer = setInterval(load, POLL_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [workspaceId, jobId]);
  if (position === null) return null;
  return <p className="queue-line">{t('chat.queue_position', { position })}</p>;
}
