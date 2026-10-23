import { readFileSync } from 'node:fs';
import { expect, test } from 'vitest';
import { emptyTranscript, foldEvent, parseEvent, transcriptMessages } from './sse';

test('the shared SSE fixture folds Thai and emoji without loss', () => {
  const bytes = readFileSync('src/testing/fixtures/thai_emoji.sse');
  let state = emptyTranscript();
  for (const block of bytes.toString('utf8').split('\n\n')) {
    const id = block.match(/^id: (\d+)$/m);
    const data = block.match(/^data: (.+)$/m);
    if (!id || !data) continue;
    const event = parseEvent(JSON.parse(data[1]!));
    expect(event).not.toBeNull();
    state = foldEvent(state, { seq: Number(id[1]), event: event! });
  }
  expect(state.lastSeq).toBe(5);
  expect(transcriptMessages(state).map((message) => message.body)).toEqual([
    'ที่นี่สวัสดีครับ 👋🏽 🇹🇭',
  ]);
});
