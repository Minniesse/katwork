import { afterEach, expect, test } from 'vitest';
import { newClientMsgId } from './client-msg-id';

const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

afterEach(() => {
  delete (crypto as { randomUUID?: unknown }).randomUUID;
});

test('uses_random_uuid_in_a_secure_context', () => {
  expect(newClientMsgId()).toMatch(UUID_V4);
});

test('builds_a_v4_uuid_from_get_random_values_without_random_uuid', () => {
  // A plain-HTTP LAN origin is not a secure context: randomUUID is missing there.
  Object.defineProperty(crypto, 'randomUUID', { value: undefined, configurable: true });
  const ids = Array.from({ length: 200 }, () => newClientMsgId());
  for (const id of ids) expect(id).toMatch(UUID_V4);
  expect(new Set(ids).size).toBe(ids.length);
  // The server accepts at most 64 characters of [A-Za-z0-9._:-] (Task 6).
  expect(ids[0]).toMatch(/^[A-Za-z0-9._:-]{1,64}$/);
});
