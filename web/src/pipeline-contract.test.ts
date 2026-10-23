import { expect, test } from 'vitest';
import { emptyTranscript, foldEvent, parseEvent, transcriptMessages } from './sse';
import answered from './testing/fixtures/pipeline-answered.json';
import cancelled from './testing/fixtures/pipeline-cancelled.json';
import needsInput from './testing/fixtures/pipeline-needs-input.json';

// Captured by the Rust pipeline contract tests; shared with the chat page's reducer tests.
for (const [outcome, frames, reply] of [
  ['answered', answered, "Hello! I am your team's AI assistant. I can help you plan and organize your work."],
  ['cancelled', cancelled, 'Partial reply '.repeat(40)],
  ['needs_input', needsInput, 'Which option?'],
] as const) {
  test(`real pipeline ${outcome} events fold into one final reply`, () => {
    let state = emptyTranscript();
    for (const frame of frames as { seq: number; event: unknown }[]) {
      const event = parseEvent(frame.event);
      expect(event).not.toBeNull();
      if (event) state = foldEvent(state, { seq: frame.seq, event });
    }
    expect(Object.values(state.jobs)).toHaveLength(1);
    const job = Object.values(state.jobs)[0];
    const messages = transcriptMessages(state);
    expect(messages).toHaveLength(2);
    const message = messages[1];
    if (!job || !message) throw new Error('pipeline must yield a job and reply');
    expect(job.status).toBe(outcome);
    expect(message.body).toBe(reply);
    expect(message.provisional).toBe(false);
    expect(Object.values(state.runs).every((run) => run.closed)).toBe(true);
    if (outcome === 'answered') expect(message.synthetic).toBe(true);
    if (outcome === 'cancelled') {
      // Task 13b keeps errors per Kat, and a cancelled close fills no slot; Stop keeps the
      // partial reply as a stopped message.
      expect(state.errors).toEqual({});
      expect(message.stopped).toBe(true);
    }
    if (outcome === 'needs_input') {
      expect(message.question).toEqual({ question: 'Which option?', options: ['One', 'Two'] });
      expect(job.reason).toBe('question_pending');
    }
    // Reconnect replay of the same sequence never duplicates a persisted message.
    for (const frame of frames as { seq: number; event: unknown }[]) {
      const event = parseEvent(frame.event);
      if (event) state = foldEvent(state, { seq: frame.seq, event });
    }
    expect(transcriptMessages(state)).toEqual(messages);
  });
}
