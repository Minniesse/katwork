// Builders for AG-UI and katwork.* events, shaped like Task 9's wire JSON. Test-only.
import type { AgUiEvent, SeqEvent } from '../sse';

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
  kwRunStarted: (runId: string, katId: string, replyMessageId: string) =>
    custom('katwork.run_started', { runId, katId, replyMessageId }),
  kwRunClosed: (runId: string, end: 'finished' | 'cancelled' | 'interrupted') =>
    custom('katwork.run_closed', { runId, end }),
  stored: (
    messageId: string,
    body: string,
    opts: {
      author?: unknown;
      synthetic?: boolean;
      question?: { question: string; options: string[] } | null;
    } = {},
  ) =>
    custom('katwork.message_stored', {
      messageId,
      author: opts.author ?? { kind: 'kat', id: 'kat-a' },
      body,
      synthetic: opts.synthetic ?? false,
      question: opts.question ?? null,
    }),
  userStored: (messageId: string, body: string) =>
    ev.stored(messageId, body, { author: { kind: 'user', id: 'user-1' } }),
  notice: (messageId: string, code: string) =>
    ev.stored(messageId, code, { author: { kind: 'system' } }),
  jobStatus: (jobId: string, status: string | null, reason: string | null = null) =>
    custom('katwork.job_status', { jobId, status, reason, holdReason: null }),
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
