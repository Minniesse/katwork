// AG-UI over SSE: a wrapper around the browser EventSource that resumes from the last seq it
// saw, and the pure reducer that folds a thread's event log into a transcript (plan Task 9
// folding rules, Task 13b's Chat page).
import { ApiError, api } from './api';

export type AgUiEvent =
  | { type: 'RUN_STARTED'; threadId: string; runId: string }
  | { type: 'RUN_FINISHED'; threadId: string; runId: string }
  | { type: 'RUN_ERROR'; message: string; code?: string | null }
  | { type: 'TEXT_MESSAGE_START'; messageId: string; role: string }
  | { type: 'TEXT_MESSAGE_CONTENT'; messageId: string; delta: string }
  | { type: 'TEXT_MESSAGE_END'; messageId: string }
  | {
      type: 'TOOL_CALL_START';
      toolCallId: string;
      toolCallName: string;
      parentMessageId?: string | null;
    }
  | { type: 'TOOL_CALL_ARGS'; toolCallId: string; delta: string }
  | { type: 'TOOL_CALL_END'; toolCallId: string }
  | { type: 'TOOL_CALL_RESULT'; messageId: string; toolCallId: string; content: string }
  | { type: 'CUSTOM'; name: string; value: unknown };

/** One event and its per-thread sequence number (the SSE `id:`). */
export interface SeqEvent {
  seq: number;
  event: AgUiEvent;
}

export type Author =
  { kind: 'user'; id: string } | { kind: 'kat'; id: string } | { kind: 'system' };

export interface Question {
  question: string;
  options: string[];
}

export interface NoticeKat {
  id: string;
  name: string;
}

/** The parameters of a `System` notice (Task 6): its code and the Kats it names. */
export interface NoticeMeta {
  code: string;
  kats: NoticeKat[];
}

export interface ChatMessage {
  id: string;
  author: Author | null;
  /** Resolved by the server: a person's display name, a Kat's name, or "" for a notice. */
  authorName: string;
  body: string;
  /** Streamed text not yet confirmed by `katwork.message_stored`. */
  provisional: boolean;
  synthetic: boolean;
  /** A Kat reply that Stop cut short (Task 12's `job::cancel`). */
  stopped: boolean;
  question: Question | null;
  meta: NoticeMeta | null;
  jobId: string | null;
  clientMsgId: string | null;
  createdAt: string | null;
  runId: string | null;
}

/** A committed message as the messages API returns it (Task 6's `Message`). */
export interface ApiMessage {
  id: string;
  author: unknown;
  author_name: string;
  body: string;
  synthetic: boolean;
  stopped: boolean;
  question: Question | null;
  meta: NoticeMeta | null;
  job_id: string | null;
  job_status: string | null;
  job_reason: string | null;
  client_msg_id: string | null;
  created_at: string;
}

export interface ToolCall {
  name: string;
  parentMessageId: string | null;
  args: string;
  argsComplete: boolean;
  result: string | null;
  ok: boolean | null;
  code: string | null;
}

export interface RunInfo {
  katId: string;
  jobId: string | null;
  replyMessageId: string;
  synthetic: boolean;
  closed: boolean;
}

export interface JobInfo {
  /** Null while the job runs or waits in the queue. */
  status: string | null;
  reason: string | null;
  holdReason: string | null;
  /** The Kat the job was routed to, once a run or a reply names it. */
  katId: string | null;
  /** A run of this job has started. */
  started: boolean;
}

export interface Transcript {
  lastSeq: number;
  /**
   * Run bookkeeping is folded up to here. It trails `lastSeq` after a snapshot, until the
   * replayed events the snapshot covers have arrived.
   */
  runsSeq: number;
  messages: Record<string, ChatMessage>;
  runs: Record<string, RunInfo>;
  toolCalls: Record<string, ToolCall>;
  katStates: Record<string, string>;
  jobs: Record<string, JobInfo>;
  /**
   * Each Kat's latest run error code, by Kat id. Runs overlap, so one Kat's error never hides
   * or clears another's; only a later answered or ready job of the same Kat clears it.
   */
  errors: Record<string, string>;
}

export type StreamStatus = 'live' | 'reconnecting' | 'polling' | 'resync' | 'unauthenticated';

/** Two missed 15 s pings plus slack. */
export const STALE_AFTER_MS = 35_000;
const BACKOFF_FIRST_MS = 1_000;
const BACKOFF_MAX_MS = 30_000;

const REQUIRED: Record<AgUiEvent['type'], readonly string[]> = {
  RUN_STARTED: ['threadId', 'runId'],
  RUN_FINISHED: ['threadId', 'runId'],
  RUN_ERROR: ['message'],
  TEXT_MESSAGE_START: ['messageId'],
  TEXT_MESSAGE_CONTENT: ['messageId', 'delta'],
  TEXT_MESSAGE_END: ['messageId'],
  TOOL_CALL_START: ['toolCallId', 'toolCallName'],
  TOOL_CALL_ARGS: ['toolCallId', 'delta'],
  TOOL_CALL_END: ['toolCallId'],
  TOOL_CALL_RESULT: ['messageId', 'toolCallId', 'content'],
  CUSTOM: ['name'],
};

// A job status that means the Kat's work went through; it clears that Kat's error.
const SUCCESS = new Set(['answered', 'ready']);
// Run endings that are outcomes, not errors.
const NOT_ERRORS = new Set(['cancelled', 'question_pending', 'approval_pending']);

type Fields = Record<string, unknown>;

const isString = (value: unknown): value is string => typeof value === 'string';
const stringOrNull = (value: unknown): string | null => (isString(value) ? value : null);

function fields(value: unknown): Fields | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Fields)
    : null;
}

function own<T>(record: Record<string, T>, key: string): T | undefined {
  return Object.hasOwn(record, key) ? record[key] : undefined;
}

/** Checks the shape of one parsed `data:` payload. */
export function parseEvent(raw: unknown): AgUiEvent | null {
  const event = fields(raw);
  if (!event || !isString(event.type) || !Object.hasOwn(REQUIRED, event.type)) return null;
  const required = REQUIRED[event.type as AgUiEvent['type']];
  return required.every((name) => isString(event[name])) ? (event as AgUiEvent) : null;
}

/** Task 6's `Author`: `{"kind": "user" | "kat", "id"}` or `{"kind": "system"}`. */
export function parseAuthor(raw: unknown): Author | null {
  const author = fields(raw);
  if (!author) return null;
  if (author.kind === 'system') return { kind: 'system' };
  if ((author.kind === 'user' || author.kind === 'kat') && isString(author.id)) {
    return { kind: author.kind, id: author.id };
  }
  return null;
}

function parseQuestion(raw: unknown): Question | null {
  const question = fields(raw);
  if (!question || !isString(question.question)) return null;
  const options = Array.isArray(question.options) ? question.options.filter(isString) : [];
  return { question: question.question, options };
}

function parseMeta(raw: unknown): NoticeMeta | null {
  const meta = fields(raw);
  if (!meta || !isString(meta.code)) return null;
  const kats = (Array.isArray(meta.kats) ? meta.kats : [])
    .map(fields)
    .filter((kat): kat is Fields => kat !== null && isString(kat.id) && isString(kat.name))
    .map((kat) => ({ id: kat.id as string, name: kat.name as string }));
  return { code: meta.code, kats };
}

function parseData(raw: unknown): Fields {
  try {
    return fields(JSON.parse(String(raw))) ?? {};
  } catch {
    return {};
  }
}

/**
 * Streams a thread's events from `options.after`. Every EventSource this opens asks for
 * `?after=<last seq seen>`, because a new EventSource never sends `Last-Event-ID`; events at
 * or below the last seq are dropped. While the browser reconnects by itself the status is
 * `reconnecting`; when it gives up (CLOSED, which any non-200 causes), the session is checked
 * and the stream reopened after a jittered backoff from 1 s doubling to 30 s. A watchdog
 * reopens a stream that sends no frame for 35 s. Returns the unsubscribe function.
 */
export function subscribe(
  path: string,
  onEvent: (event: SeqEvent) => void,
  options: { after: number; onStatus?: (status: StreamStatus) => void },
): () => void {
  let lastSeq = options.after;
  let source: EventSource | null = null;
  let status: StreamStatus | null = null;
  // Over the session's stream budget: the page polls, the EventSource retries every 30 s.
  let polling = false;
  let attempt = 0;
  let stopped = false;
  let reopen: ReturnType<typeof setTimeout> | undefined;
  let watchdog: ReturnType<typeof setTimeout> | undefined;

  const report = (next: StreamStatus) => {
    if (stopped || next === status) return;
    status = next;
    options.onStatus?.(next);
  };
  const stop = () => {
    stopped = true;
    clearTimeout(reopen);
    clearTimeout(watchdog);
    source?.close();
  };
  const watch = () => {
    clearTimeout(watchdog);
    if (polling) return;
    watchdog = setTimeout(() => {
      report('reconnecting');
      open();
    }, STALE_AFTER_MS);
  };
  const alive = () => {
    attempt = 0;
    polling = false;
    report('live');
    watch();
  };

  const open = () => {
    if (stopped) return;
    clearTimeout(reopen);
    source?.close();
    const current = new EventSource(`${path}?after=${lastSeq}`);
    source = current;
    // Handlers of a stream this wrapper already replaced or closed do nothing.
    const mine =
      <E extends Event>(handler: (event: E) => void) =>
      (event: E) => {
        if (!stopped && source === current) handler(event);
      };

    current.onopen = mine(() => {
      if (!polling) alive();
    });
    current.onmessage = mine((message: MessageEvent) => {
      alive();
      const seq = Number(message.lastEventId);
      if (!Number.isSafeInteger(seq) || seq <= lastSeq) return;
      let data: unknown;
      try {
        data = JSON.parse(String(message.data));
      } catch {
        return;
      }
      const event = parseEvent(data);
      if (!event) return;
      lastSeq = seq;
      onEvent({ seq, event });
    });
    current.addEventListener('ping', mine(alive));
    current.addEventListener(
      'resync_required',
      mine(() => {
        report('resync');
        stop();
      }),
    );
    current.addEventListener(
      'stream_error',
      mine((event: MessageEvent) => {
        switch (parseData(event.data).code) {
          case 'too_many_streams':
            // The EventSource stays: its own reconnect tries again after the server's 30 s retry.
            polling = true;
            clearTimeout(watchdog);
            report('polling');
            return;
          case 'unauthenticated':
            report('unauthenticated');
            stop();
            return;
          case 'not_found':
            stop();
            return;
          default:
            // The server ends the stream; the browser reconnects with Last-Event-ID.
            report('reconnecting');
        }
      }),
    );
    current.onerror = mine(() => {
      if (current.readyState !== EventSource.CLOSED) {
        // A dropped stream (the server's write deadline, a network blip): the browser retries.
        if (!polling) report('reconnecting');
        return;
      }
      polling = false;
      clearTimeout(watchdog);
      report('reconnecting');
      void api
        .me()
        .then(
          () => true,
          (error: unknown) => !(error instanceof ApiError && error.status === 401),
        )
        .then((signedIn) => {
          if (stopped || source !== current) return;
          if (!signedIn) {
            report('unauthenticated');
            stop();
            return;
          }
          const ceiling = Math.min(BACKOFF_MAX_MS, BACKOFF_FIRST_MS * 2 ** attempt);
          attempt += 1;
          reopen = setTimeout(open, ceiling / 2 + Math.random() * (ceiling / 2));
        });
    });
    watch();
  };

  open();
  return stop;
}

export function emptyTranscript(): Transcript {
  return {
    lastSeq: 0,
    runsSeq: 0,
    messages: {},
    runs: {},
    toolCalls: {},
    katStates: {},
    jobs: {},
    errors: {},
  };
}

function emptyJob(): JobInfo {
  return { status: null, reason: null, holdReason: null, katId: null, started: false };
}

function fromApi(message: ApiMessage): ChatMessage {
  return {
    id: message.id,
    author: parseAuthor(message.author),
    authorName: message.author_name,
    body: message.body,
    provisional: false,
    synthetic: message.synthetic,
    stopped: message.stopped,
    question: parseQuestion(message.question),
    meta: parseMeta(message.meta),
    jobId: message.job_id,
    clientMsgId: message.client_msg_id,
    createdAt: message.created_at,
    runId: null,
  };
}

/**
 * Puts stored messages from the messages API, replacing what the transcript holds for each id,
 * and takes each message's job status as the job's current one.
 */
export function mergeMessages(state: Transcript, messages: ApiMessage[]): Transcript {
  return messages.reduce((next, message) => {
    const withMessage = putMessage(next, {
      ...fromApi(message),
      runId: own(next.messages, message.id)?.runId ?? null,
    });
    if (!message.job_id) return withMessage;
    const job = own(withMessage.jobs, message.job_id) ?? emptyJob();
    const author = parseAuthor(message.author);
    return {
      ...withMessage,
      jobs: {
        ...withMessage.jobs,
        [message.job_id]: {
          ...job,
          status: message.job_status,
          reason: message.job_reason,
          holdReason: message.job_status === null ? job.holdReason : null,
          katId: author?.kind === 'kat' ? author.id : job.katId,
        },
      },
    };
  }, state);
}

/**
 * Starts from the messages API's snapshot at `lastSeq` (its `event_seq`). Events up to and
 * including `lastSeq` are skipped, except that their `katwork.run_started` and
 * `katwork.run_closed` still record which runs are open.
 */
export function transcriptFromSnapshot(messages: ApiMessage[], lastSeq: number): Transcript {
  return mergeMessages({ ...emptyTranscript(), lastSeq }, messages);
}

/**
 * The person's own post, as the post route confirmed it. It is added only when neither the
 * stream nor a reload has put that message yet, and its job counts as running until an event
 * says otherwise.
 */
export function putPosted(
  state: Transcript,
  posted: { id: string; author: Author; body: string; clientMsgId: string; jobId: string | null },
): Transcript {
  if (Object.hasOwn(state.messages, posted.id)) return state;
  const next = putMessage(state, {
    id: posted.id,
    author: posted.author,
    authorName: '',
    body: posted.body,
    provisional: false,
    synthetic: false,
    stopped: false,
    question: null,
    meta: null,
    jobId: posted.jobId,
    clientMsgId: posted.clientMsgId,
    createdAt: new Date().toISOString(),
    runId: null,
  });
  if (!posted.jobId || Object.hasOwn(next.jobs, posted.jobId)) return next;
  return { ...next, jobs: { ...next.jobs, [posted.jobId]: emptyJob() } };
}

/**
 * Every bubble, stored or provisional, in message id order, the order `message::list` uses:
 * UUIDv7 ids in canonical lowercase form sort in time order as strings. A provisional reply
 * takes its place from its run's `replyMessageId`, so the order is the same live and after
 * a reload.
 */
export function transcriptMessages(state: Transcript): ChatMessage[] {
  return Object.keys(state.messages)
    .sort()
    .map((id) => state.messages[id]!);
}

/** The jobs of the thread that are still running or queued. */
export function runningJobs(state: Transcript): string[] {
  return Object.entries(state.jobs)
    .filter(([, job]) => job.status === null)
    .map(([id]) => id);
}

/**
 * Folds one event into the transcript. Pure and deterministic: replaying the log from any
 * seq ends at the same state, because an event at or below `runsSeq` is a no-op and one at
 * or below `lastSeq` only does run bookkeeping.
 */
export function foldEvent(state: Transcript, { seq, event }: SeqEvent): Transcript {
  if (seq > state.lastSeq) return { ...apply(state, event), lastSeq: seq, runsSeq: seq };
  if (seq > state.runsSeq) return { ...applyRunBookkeeping(state, event), runsSeq: seq };
  return state;
}

/**
 * A replayed event the snapshot already covers. Only run starts and closes count; the
 * transcript holds no provisional text yet, so they change nothing but runs and jobs.
 */
function applyRunBookkeeping(state: Transcript, event: AgUiEvent): Transcript {
  if (event.type !== 'CUSTOM') return state;
  if (event.name !== 'katwork.run_started' && event.name !== 'katwork.run_closed') return state;
  return applyCustom(state, event.name, fields(event.value) ?? {});
}

function apply(state: Transcript, event: AgUiEvent): Transcript {
  switch (event.type) {
    case 'TEXT_MESSAGE_START':
      return startText(state, event.messageId);
    case 'TEXT_MESSAGE_CONTENT':
      return appendText(state, event.messageId, event.delta);
    case 'TOOL_CALL_START':
      return putToolCall(state, event.toolCallId, {
        name: event.toolCallName,
        parentMessageId: event.parentMessageId ?? null,
        args: '',
        argsComplete: false,
        result: null,
        ok: null,
        code: null,
      });
    case 'TOOL_CALL_ARGS':
      return updateToolCall(state, event.toolCallId, (call) => ({
        args: call.args + event.delta,
      }));
    case 'TOOL_CALL_END':
      return updateToolCall(state, event.toolCallId, () => ({ argsComplete: true }));
    case 'TOOL_CALL_RESULT':
      return updateToolCall(state, event.toolCallId, () => ({ result: event.content }));
    case 'CUSTOM':
      return applyCustom(state, event.name, fields(event.value) ?? {});
    case 'RUN_ERROR':
    // RUN_ERROR carries no run id, so it never decides anything: the `katwork.run_closed`
    // right before it names the run and carries the same code (Task 9).
    case 'RUN_STARTED':
    case 'RUN_FINISHED':
    case 'TEXT_MESSAGE_END':
      return state;
  }
}

function applyCustom(state: Transcript, name: string, value: Fields): Transcript {
  switch (name) {
    case 'katwork.run_started': {
      const { runId, katId, replyMessageId } = value;
      if (!isString(runId) || !isString(katId) || !isString(replyMessageId)) return state;
      const jobId = stringOrNull(value.jobId);
      const synthetic = value.synthetic === true;
      let next: Transcript = {
        ...state,
        runs: {
          ...state.runs,
          [runId]: { katId, jobId, replyMessageId, synthetic, closed: false },
        },
      };
      if (jobId) {
        const job = own(state.jobs, jobId) ?? emptyJob();
        next = { ...next, jobs: { ...next.jobs, [jobId]: { ...job, katId, started: true } } };
      }
      const reply = own(state.messages, replyMessageId);
      return reply?.provisional
        ? putMessage(next, { ...reply, runId, synthetic, author: { kind: 'kat', id: katId } })
        : next;
    }
    case 'katwork.run_closed': {
      const { runId } = value;
      const run = isString(runId) ? own(state.runs, runId) : undefined;
      if (!isString(runId) || !run) return state;
      let next: Transcript = {
        ...state,
        runs: { ...state.runs, [runId]: { ...run, closed: true } },
      };
      const code = stringOrNull(value.code);
      if (code !== null && !NOT_ERRORS.has(code)) {
        next = { ...next, errors: { ...next.errors, [run.katId]: code } };
      }
      const reply = own(next.messages, run.replyMessageId);
      // Unconfirmed text from a closed run never stays beside the stored transcript.
      return reply?.provisional && (reply.runId === runId || reply.runId === null)
        ? dropMessage(next, reply.id)
        : next;
    }
    case 'katwork.message_stored': {
      const { messageId, body } = value;
      if (!isString(messageId) || !isString(body)) return state;
      const existing = own(state.messages, messageId);
      const author = parseAuthor(value.author) ?? existing?.author ?? null;
      const jobId = stringOrNull(value.jobId);
      const next = putMessage(state, {
        id: messageId,
        author,
        authorName: stringOrNull(value.authorName) ?? existing?.authorName ?? '',
        body,
        provisional: false,
        synthetic: value.synthetic === true,
        stopped: value.stopped === true,
        question: parseQuestion(value.question),
        meta: parseMeta(value.meta),
        jobId,
        clientMsgId: stringOrNull(value.clientMsgId),
        createdAt: stringOrNull(value.createdAt) ?? existing?.createdAt ?? null,
        runId: existing?.runId ?? null,
      });
      if (!jobId || author?.kind !== 'kat') return next;
      const job = own(next.jobs, jobId) ?? emptyJob();
      return job.katId
        ? next
        : { ...next, jobs: { ...next.jobs, [jobId]: { ...job, katId: author.id } } };
    }
    case 'katwork.job_status': {
      const { jobId } = value;
      if (!isString(jobId)) return state;
      const existing = own(state.jobs, jobId) ?? emptyJob();
      const job: JobInfo = {
        ...existing,
        status: stringOrNull(value.status),
        reason: stringOrNull(value.reason),
        holdReason: stringOrNull(value.holdReason),
      };
      const next = { ...state, jobs: { ...state.jobs, [jobId]: job } };
      if (job.status === null || !SUCCESS.has(job.status) || !job.katId) return next;
      if (!Object.hasOwn(next.errors, job.katId)) return next;
      const errors = { ...next.errors };
      delete errors[job.katId];
      return { ...next, errors };
    }
    case 'katwork.kat_state': {
      const { katId, state: katState } = value;
      if (!isString(katId) || !isString(katState)) return state;
      return { ...state, katStates: { ...state.katStates, [katId]: katState } };
    }
    case 'katwork.tool_status': {
      const { toolCallId, ok } = value;
      if (!isString(toolCallId) || typeof ok !== 'boolean') return state;
      return updateToolCall(state, toolCallId, () => ({
        ok,
        code: isString(value.code) ? value.code : null,
      }));
    }
    default:
      // katwork.decision, katwork.usage and anything newer don't change the transcript.
      return state;
  }
}

/** A turn's TEXT_MESSAGE_START starts that message's provisional text over. */
function startText(state: Transcript, messageId: string): Transcript {
  const existing = own(state.messages, messageId);
  if (existing && !existing.provisional) return state;
  const run = openRunFor(state, messageId);
  return putMessage(state, {
    id: messageId,
    author: existing?.author ?? (run ? { kind: 'kat', id: run.info.katId } : null),
    authorName: existing?.authorName ?? '',
    body: '',
    provisional: true,
    synthetic: run?.info.synthetic ?? existing?.synthetic ?? false,
    stopped: false,
    question: null,
    meta: null,
    jobId: run?.info.jobId ?? existing?.jobId ?? null,
    clientMsgId: null,
    createdAt: null,
    runId: existing?.runId ?? run?.id ?? null,
  });
}

/**
 * Appends a delta as received: the server's coalescer never cuts a grapheme cluster, so the
 * text is never split or revealed per character here. A delta for a message whose
 * TEXT_MESSAGE_START this client never saw (it attached mid-run) is ignored.
 */
function appendText(state: Transcript, messageId: string, delta: string): Transcript {
  const existing = own(state.messages, messageId);
  if (!existing?.provisional) return state;
  return putMessage(state, { ...existing, body: existing.body + delta });
}

function openRunFor(state: Transcript, messageId: string): { id: string; info: RunInfo } | null {
  let found: { id: string; info: RunInfo } | null = null;
  for (const [id, info] of Object.entries(state.runs)) {
    if (info.replyMessageId === messageId && !info.closed) found = { id, info };
  }
  return found;
}

function putMessage(state: Transcript, message: ChatMessage): Transcript {
  return { ...state, messages: { ...state.messages, [message.id]: message } };
}

function dropMessage(state: Transcript, id: string): Transcript {
  const messages = { ...state.messages };
  delete messages[id];
  return { ...state, messages };
}

function putToolCall(state: Transcript, id: string, call: ToolCall): Transcript {
  return { ...state, toolCalls: { ...state.toolCalls, [id]: call } };
}

function updateToolCall(
  state: Transcript,
  id: string,
  change: (call: ToolCall) => Partial<ToolCall>,
): Transcript {
  const call = own(state.toolCalls, id) ?? {
    name: '',
    parentMessageId: null,
    args: '',
    argsComplete: false,
    result: null,
    ok: null,
    code: null,
  };
  return putToolCall(state, id, { ...call, ...change(call) });
}
