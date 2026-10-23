// Builders for AG-UI and katwork.* events, shaped like Task 9's wire JSON. Test-only.
import type { AgUiEvent, ApiMessage, SeqEvent } from '../sse';

const custom = (name: string, value: unknown): AgUiEvent => ({ type: 'CUSTOM', name, value });

export const ev = {
  runStarted: (runId: string, threadId = 'thread-1'): AgUiEvent => ({
    type: 'RUN_STARTED',
    threadId,
    runId,
  }),
  runFinished: (runId: string, threadId = 'thread-1'): AgUiEvent => ({
    type: 'RUN_FINISHED',
    threadId,
    runId,
  }),
  runError: (code: string): AgUiEvent => ({ type: 'RUN_ERROR', message: code, code }),
  textStart: (messageId: string): AgUiEvent => ({
    type: 'TEXT_MESSAGE_START',
    messageId,
    role: 'assistant',
  }),
  text: (messageId: string, delta: string): AgUiEvent => ({
    type: 'TEXT_MESSAGE_CONTENT',
    messageId,
    delta,
  }),
  textEnd: (messageId: string): AgUiEvent => ({ type: 'TEXT_MESSAGE_END', messageId }),
  toolStart: (toolCallId: string, toolCallName: string, parentMessageId: string): AgUiEvent => ({
    type: 'TOOL_CALL_START',
    toolCallId,
    toolCallName,
    parentMessageId,
  }),
  toolArgs: (toolCallId: string, delta: string): AgUiEvent => ({
    type: 'TOOL_CALL_ARGS',
    toolCallId,
    delta,
  }),
  toolEnd: (toolCallId: string): AgUiEvent => ({ type: 'TOOL_CALL_END', toolCallId }),
  toolResult: (messageId: string, toolCallId: string, content: string): AgUiEvent => ({
    type: 'TOOL_CALL_RESULT',
    messageId,
    toolCallId,
    content,
  }),
  kwRunStarted: (
    runId: string,
    katId: string,
    replyMessageId: string,
    opts: { jobId?: string; synthetic?: boolean } = {},
  ) =>
    custom('katwork.run_started', {
      runId,
      jobId: opts.jobId ?? `job-of-${runId}`,
      katId,
      replyMessageId,
      synthetic: opts.synthetic ?? false,
    }),
  kwRunClosed: (
    runId: string,
    end: 'finished' | 'cancelled' | 'interrupted',
    code: string | null = null,
  ) => custom('katwork.run_closed', { runId, end, code }),
  stored: (
    messageId: string,
    body: string,
    opts: {
      author?: unknown;
      authorName?: string;
      synthetic?: boolean;
      stopped?: boolean;
      question?: { question: string; options: string[] } | null;
      meta?: { code: string; kats: { id: string; name: string }[] } | null;
      jobId?: string | null;
      clientMsgId?: string | null;
    } = {},
  ) =>
    custom('katwork.message_stored', {
      messageId,
      author: opts.author ?? { kind: 'kat', id: 'kat-a' },
      authorName: opts.authorName ?? '',
      body,
      synthetic: opts.synthetic ?? false,
      stopped: opts.stopped ?? false,
      question: opts.question ?? null,
      meta: opts.meta ?? null,
      jobId: opts.jobId ?? null,
      clientMsgId: opts.clientMsgId ?? null,
      createdAt: '2026-10-23T05:00:00Z',
    }),
  userStored: (
    messageId: string,
    body: string,
    opts: { userId?: string; authorName?: string; jobId?: string; clientMsgId?: string } = {},
  ) =>
    ev.stored(messageId, body, {
      author: { kind: 'user', id: opts.userId ?? 'user-1' },
      authorName: opts.authorName ?? 'Person',
      jobId: opts.jobId ?? null,
      clientMsgId: opts.clientMsgId ?? null,
    }),
  notice: (messageId: string, code: string, kats: { id: string; name: string }[] = []) =>
    ev.stored(messageId, code, { author: { kind: 'system' }, meta: { code, kats } }),
  jobStatus: (
    jobId: string,
    status: string | null,
    reason: string | null = null,
    holdReason: string | null = null,
  ) => custom('katwork.job_status', { jobId, status, reason, holdReason }),
  katState: (katId: string, state: string) => custom('katwork.kat_state', { katId, state }),
  decision: (jobId: string) =>
    custom('katwork.decision', {
      jobId,
      inputs: {},
      verdict: {},
      policyVersion: 1,
      thresholds: {},
    }),
  toolStatus: (toolCallId: string, ok: boolean, code: string | null = null) =>
    custom('katwork.tool_status', { toolCallId, ok, code }),
};

/** Numbers events as the server's per-thread log would, starting at `first`. */
export function numbered(events: AgUiEvent[], first = 1): SeqEvent[] {
  return events.map((event, i) => ({ seq: first + i, event }));
}

/** A message as the messages API returns it (Task 6's `Message`). */
export function apiMessage(
  id: string,
  body: string,
  opts: Partial<Omit<ApiMessage, 'id' | 'body'>> = {},
): ApiMessage {
  return {
    id,
    author: { kind: 'kat', id: 'kat-a' },
    author_name: '',
    body,
    synthetic: false,
    stopped: false,
    question: null,
    meta: null,
    job_id: null,
    job_status: null,
    job_reason: null,
    client_msg_id: null,
    created_at: '2026-10-23T05:00:00Z',
    ...opts,
  };
}
