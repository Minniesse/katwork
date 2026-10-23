// AG-UI over SSE: the browser EventSource with its native Last-Event-ID resume, and the pure
// reducer that folds a thread's event log into a transcript (plan Task 9 folding rules).

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

export type Author = { kind: 'user'; id: string } | { kind: 'dot'; id: string } | { kind: 'system' };

export interface Question {
  question: string;
  options: string[];
}

export interface ChatMessage {
  id: string;
  author: Author | null;
  body: string;
  /** Streamed text not yet confirmed by `katwork.message_stored`. */
  provisional: boolean;
  synthetic: boolean;
  question: Question | null;
  runId: string | null;
}

/** A committed message, as the messages API returns it. */
export interface StoredMessage {
  id: string;
  author: Author;
  body: string;
  synthetic: boolean;
  question: Question | null;
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
  dotId: string;
  replyMessageId: string;
  closed: boolean;
}

export interface JobInfo {
  status: string | null;
  reason: string | null;
  holdReason: string | null;
}

export interface Transcript {
  lastSeq: number;
  /**
   * Run bookkeeping is folded up to here. It trails `lastSeq` after a snapshot, until the
   * replayed events the snapshot covers have arrived.
   */
  runsSeq: number;
  order: string[];
  messages: Record<string, ChatMessage>;
  runs: Record<string, RunInfo>;
  toolCalls: Record<string, ToolCall>;
  dotStates: Record<string, string>;
  jobs: Record<string, JobInfo>;
  /** The code of the latest `RUN_ERROR`, until a later job succeeds. */
  runError: string | null;
}

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

// A job status that means the work went through; it clears an earlier run's error.
const SUCCESS = new Set(['accepted', 'ready', 'answered', 'needs_input']);

type Fields = Record<string, unknown>;

const isString = (value: unknown): value is string => typeof value === 'string';

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

// TODO(13b): pin to Task 6's serde shape for `Author` once it lands; until then both the
// adjacently tagged `{kind, id}` and serde's default `{"Dot": id}` / `"System"` are read.
export function parseAuthor(raw: unknown): Author | null {
  if (raw === 'System' || raw === 'system') return { kind: 'system' };
  const author = fields(raw);
  if (!author) return null;
  const kind = isString(author.kind) ? author.kind.toLowerCase() : null;
  if (kind === 'system') return { kind: 'system' };
  if ((kind === 'user' || kind === 'dot') && isString(author.id)) return { kind, id: author.id };
  if (isString(author.User)) return { kind: 'user', id: author.User };
  if (isString(author.Dot)) return { kind: 'dot', id: author.Dot };
  return null;
}

function parseQuestion(raw: unknown): Question | null {
  const question = fields(raw);
  if (!question || !isString(question.question)) return null;
  const options = Array.isArray(question.options) ? question.options.filter(isString) : [];
  return { question: question.question, options };
}

/**
 * Streams a thread's events. The browser reconnects by itself and resumes with
 * `Last-Event-ID`; when it gives up (the stream is CLOSED), a new EventSource is opened after
 * a backoff, and the replay it gets is de-duplicated by seq. Returns the unsubscribe function.
 */
export function subscribe(url: string, onEvent: (event: SeqEvent) => void): () => void {
  let lastSeq = 0;
  let source: EventSource | null = null;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let attempt = 0;
  let stopped = false;

  const open = () => {
    const current = new EventSource(url);
    source = current;
    current.onopen = () => {
      attempt = 0;
    };
    current.onmessage = (message: MessageEvent) => {
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
    };
    current.onerror = () => {
      // While CONNECTING the browser retries on its own; only a CLOSED stream needs us.
      if (stopped || current.readyState !== EventSource.CLOSED) return;
      const delay = Math.min(30_000, 1_000 * 2 ** attempt);
      attempt += 1;
      retry = setTimeout(open, delay);
    };
  };

  open();
  return () => {
    stopped = true;
    clearTimeout(retry);
    source?.close();
  };
}

export function emptyTranscript(): Transcript {
  return {
    lastSeq: 0,
    runsSeq: 0,
    order: [],
    messages: {},
    runs: {},
    toolCalls: {},
    dotStates: {},
    jobs: {},
    runError: null,
  };
}

/**
 * Starts from the messages API's snapshot. Events up to and including `lastSeq` are skipped,
 * except that their `katwork.run_started` and `katwork.run_closed` still record which runs
 * are open: the snapshot holds stored messages only, and text a run streams after it must
 * still be dropped when that run closes.
 */
export function transcriptFromSnapshot(messages: StoredMessage[], lastSeq: number): Transcript {
  return messages.reduce(
    (state, message) => putMessage(state, { ...message, provisional: false, runId: null }),
    { ...emptyTranscript(), lastSeq },
  );
}

export function transcriptMessages(state: Transcript): ChatMessage[] {
  return state.order.map((id) => state.messages[id]!);
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
 * transcript holds no provisional text yet, so they change nothing but `runs`.
 */
function applyRunBookkeeping(state: Transcript, event: AgUiEvent): Transcript {
  if (event.type !== 'CUSTOM') return state;
  if (event.name !== 'katwork.run_started' && event.name !== 'katwork.run_closed') return state;
  return applyCustom(state, event.name, fields(event.value) ?? {});
}

function apply(state: Transcript, event: AgUiEvent): Transcript {
  switch (event.type) {
    case 'TEXT_MESSAGE_START':
      return streamText(state, event.messageId, '', true);
    case 'TEXT_MESSAGE_CONTENT':
      return streamText(state, event.messageId, event.delta, false);
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
    case 'RUN_ERROR':
      // RUN_ERROR carries no run id, so it never decides which run's text to drop;
      // katwork.run_closed right after it does.
      return { ...state, runError: event.code || 'unknown' };
    case 'CUSTOM':
      return applyCustom(state, event.name, fields(event.value) ?? {});
    case 'RUN_STARTED':
    case 'RUN_FINISHED':
    case 'TEXT_MESSAGE_END':
      return state;
  }
}

function applyCustom(state: Transcript, name: string, value: Fields): Transcript {
  switch (name) {
    case 'katwork.run_started': {
      const { runId, dotId, replyMessageId } = value;
      if (!isString(runId) || !isString(dotId) || !isString(replyMessageId)) return state;
      const next = { ...state, runs: { ...state.runs, [runId]: { dotId, replyMessageId, closed: false } } };
      const reply = own(state.messages, replyMessageId);
      return reply?.provisional
        ? putMessage(next, { ...reply, runId, author: { kind: 'dot', id: dotId } })
        : next;
    }
    case 'katwork.run_closed': {
      const { runId } = value;
      const run = isString(runId) ? own(state.runs, runId) : undefined;
      if (!isString(runId) || !run) return state;
      const next = { ...state, runs: { ...state.runs, [runId]: { ...run, closed: true } } };
      const reply = own(state.messages, run.replyMessageId);
      // Unconfirmed text from a closed run never stays beside the stored transcript.
      return reply?.provisional && (reply.runId === runId || reply.runId === null)
        ? dropMessage(next, reply.id)
        : next;
    }
    case 'katwork.message_stored': {
      const { messageId, body } = value;
      if (!isString(messageId) || !isString(body)) return state;
      const existing = own(state.messages, messageId);
      return putMessage(state, {
        id: messageId,
        author: parseAuthor(value.author) ?? existing?.author ?? null,
        body,
        provisional: false,
        synthetic: value.synthetic === true,
        question: parseQuestion(value.question),
        runId: existing?.runId ?? null,
      });
    }
    case 'katwork.job_status': {
      const { jobId, status } = value;
      if (!isString(jobId)) return state;
      const job: JobInfo = {
        status: isString(status) ? status : null,
        reason: isString(value.reason) ? value.reason : null,
        holdReason: isString(value.holdReason) ? value.holdReason : null,
      };
      return {
        ...state,
        jobs: { ...state.jobs, [jobId]: job },
        runError: job.status !== null && SUCCESS.has(job.status) ? null : state.runError,
      };
    }
    case 'katwork.dot_state': {
      const { dotId, state: dotState } = value;
      if (!isString(dotId) || !isString(dotState)) return state;
      return { ...state, dotStates: { ...state.dotStates, [dotId]: dotState } };
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
function streamText(state: Transcript, messageId: string, delta: string, restart: boolean): Transcript {
  const existing = own(state.messages, messageId);
  if (existing && !existing.provisional) return state;
  const run = openRunFor(state, messageId);
  return putMessage(state, {
    id: messageId,
    author: existing?.author ?? (run ? { kind: 'dot', id: run.info.dotId } : null),
    body: (restart || !existing ? '' : existing.body) + delta,
    provisional: true,
    synthetic: false,
    question: null,
    runId: existing?.runId ?? run?.id ?? null,
  });
}

function openRunFor(state: Transcript, messageId: string): { id: string; info: RunInfo } | null {
  let found: { id: string; info: RunInfo } | null = null;
  for (const [id, info] of Object.entries(state.runs)) {
    if (info.replyMessageId === messageId && !info.closed) found = { id, info };
  }
  return found;
}

function putMessage(state: Transcript, message: ChatMessage): Transcript {
  const order = Object.hasOwn(state.messages, message.id) ? state.order : [...state.order, message.id];
  return { ...state, order, messages: { ...state.messages, [message.id]: message } };
}

function dropMessage(state: Transcript, id: string): Transcript {
  const messages = { ...state.messages };
  delete messages[id];
  return { ...state, messages, order: state.order.filter((other) => other !== id) };
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
