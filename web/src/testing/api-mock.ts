// Test-only fetch stub routed by method and path. Each route answers with one reply, a list of
// replies used in order (the last one repeats), or a function of the call.
import { vi } from 'vitest';

export type Reply = [status: number, body?: unknown];

export interface Call {
  method: string;
  path: string;
  body: unknown;
}

type Handler = Reply | Reply[] | ((call: Call) => Reply | Promise<Reply>);

const isReply = (handler: Reply | Reply[]): handler is Reply => typeof handler[0] === 'number';

export function mockApi(routes: Record<string, Handler>) {
  const table = new Map(Object.entries(routes));
  const calls: Call[] = [];
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const method = init?.method ?? 'GET';
    const path = String(input);
    const body = init?.body === undefined ? undefined : JSON.parse(String(init.body));
    const call = { method, path, body };
    calls.push(call);
    const handler = table.get(`${method} ${path.split('?')[0]}`);
    let reply: Reply;
    if (handler === undefined) reply = [404, { code: 'not_found' }];
    else if (typeof handler === 'function') reply = await handler(call);
    else if (isReply(handler)) reply = handler;
    else reply = handler.length > 1 ? handler.shift()! : handler[0]!;
    const [status, data] = reply;
    return new Response(data === undefined ? null : JSON.stringify(data), {
      status,
      headers: data === undefined ? {} : { 'content-type': 'application/json' },
    });
  });
  vi.stubGlobal('fetch', fetchMock);
  return {
    calls,
    /** Calls to one route, as `METHOD /path`, query strings ignored. */
    callsTo: (route: string) =>
      calls.filter((call) => `${call.method} ${call.path.split('?')[0]}` === route),
    route: (key: string, handler: Handler) => table.set(key, handler),
  };
}

/** A promise with its resolve function, to hold a request open. */
export function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
