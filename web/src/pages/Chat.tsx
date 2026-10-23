import { useEffect, useReducer, useRef, useState, type FormEvent } from 'react';
import { api, ApiError, errorMessage, type MessagePage, type Thread } from '../api';
import { Composer } from '../components/Composer';
import { PageTitle } from '../components/Layout';
import { Transcript, type PendingMessage } from '../components/Transcript';
import { hasMessage, t, useLocale } from '../i18n';
import { Link, navigate } from '../router';
import {
  emptyTranscript,
  foldEvent,
  mergeMessages,
  putPosted,
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
                {thread.title || t('chat.untitled')}
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
  | { type: 'merge'; messages: ApiMessage[] }
  | { type: 'posted'; posted: Parameters<typeof putPosted>[1] };

function reduce(state: TranscriptState, action: Action): TranscriptState {
  switch (action.type) {
    case 'snapshot':
      return transcriptFromSnapshot(action.page.messages, action.page.event_seq);
    case 'event':
      return foldEvent(state, action.event);
    case 'merge':
      return mergeMessages(state, action.messages);
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
  const [transcript, dispatch] = useReducer(reduce, undefined, emptyTranscript);
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
    if (snapshots === 0 || !streaming) return;
    setStatus(null);
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

  // Over the session's stream budget: stored messages, job statuses and Stop stay current.
  useEffect(() => {
    if (status !== 'polling') return;
    const timer = setInterval(() => {
      api.messages(workspaceId, threadId).then(
        (page) => dispatch({ type: 'merge', messages: page.messages }),
        () => {},
      );
    }, POLL_MS);
    return () => clearInterval(timer);
  }, [status, workspaceId, threadId]);

  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const onChange = () => {
      clearTimeout(timer);
      if (document.visibilityState === 'hidden')
        timer = setTimeout(() => setStreaming(false), HIDDEN_CLOSE_MS);
      else setStreaming(true);
    };
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
    const post = { clientMsgId: crypto.randomUUID(), body, failure: null };
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
  };

  const running = runningJobs(transcript).filter((job) => !finished.has(job));
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

  const replying = Object.entries(transcript.katStates)
    .filter(([, state]) => state === 'working' || state === 'researching')
    .map(([katId]) => katId);
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
        <PageTitle>{thread.title || t('chat.untitled')}</PageTitle>
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
      />
      <div className="chat-status">
        <div className="kat-status" role="status" data-testid="kat-status">
          {replying.map((katId) => (
            <p key={`replying-${katId}`} className="kat-status-line">
              {t('chat.replying', { kat: nameOf(katId) })}
            </p>
          ))}
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
        <Composer onSend={send} mentions={members.map(({ id, name }) => ({ id, name }))} />
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
