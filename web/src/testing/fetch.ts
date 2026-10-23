// Test-only fetch stub: answers each call from a list of [status, body] replies, in order.
import { vi } from 'vitest';

export type Reply = [status: number, body?: unknown];

export function stubFetch(...replies: Reply[]) {
  const queue = [...replies];
  const fetchMock = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => {
    const [status, body] = queue.length > 1 ? queue.shift()! : queue[0]!;
    return new Response(body === undefined ? null : JSON.stringify(body), {
      status,
      headers: body === undefined ? {} : { 'content-type': 'application/json' },
    });
  });
  vi.stubGlobal('fetch', fetchMock);
  return fetchMock;
}

export function sentJson(fetchMock: ReturnType<typeof stubFetch>, call = 0): unknown {
  return JSON.parse(String(fetchMock.mock.calls[call]![1]?.body));
}
