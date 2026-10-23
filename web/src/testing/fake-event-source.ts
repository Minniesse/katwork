// Test-only EventSource. Frames go in as SSE text or bytes and come out through
// eventsource-parser, the way a browser parses a stream, so tests exercise the wire format
// rather than hand-built MessageEvents.
import { createParser, type EventSourceMessage, type EventSourceParser } from 'eventsource-parser';
import { vi } from 'vitest';
import type { SeqEvent } from '../sse';

export class FakeEventSource extends EventTarget {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSED = 2;
  static instances: FakeEventSource[] = [];

  readonly CONNECTING = 0;
  readonly OPEN = 1;
  readonly CLOSED = 2;
  readyState = FakeEventSource.CONNECTING;
  lastEventId = '';
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  private parser: EventSourceParser;
  private decoder = new TextDecoder('utf-8');

  constructor(
    readonly url: string,
    readonly init?: EventSourceInit,
  ) {
    super();
    FakeEventSource.instances.push(this);
    this.parser = createParser({ onEvent: (message) => this.deliver(message) });
  }

  /** Every instance not closed yet. */
  static live(): FakeEventSource[] {
    return FakeEventSource.instances.filter(
      (source) => source.readyState !== FakeEventSource.CLOSED,
    );
  }

  static latest(): FakeEventSource {
    const source = FakeEventSource.instances.at(-1);
    if (!source) throw new Error('no EventSource was opened');
    return source;
  }

  /** The `after` query parameter the page opened this stream with. */
  get after(): string | null {
    return new URL(this.url, 'http://localhost').searchParams.get('after');
  }

  /** The server answered 200. */
  open(): void {
    if (this.readyState === FakeEventSource.CLOSED) return;
    this.readyState = FakeEventSource.OPEN;
    const event = new Event('open');
    this.onopen?.(event);
    this.dispatchEvent(event);
  }

  /** One network chunk of SSE text. */
  feed(text: string): void {
    if (this.readyState === FakeEventSource.CLOSED) return;
    if (this.readyState === FakeEventSource.CONNECTING) this.open();
    this.parser.feed(text);
  }

  /** One network chunk of bytes, decoded as a stream so a split character waits for the rest. */
  feedBytes(bytes: Uint8Array): void {
    this.feed(this.decoder.decode(bytes, { stream: true }));
  }

  emit({ seq, event }: SeqEvent): void {
    this.feed(`id: ${seq}\ndata: ${JSON.stringify(event)}\n\n`);
  }

  named(name: string, data: unknown): void {
    this.feed(`event: ${name}\ndata: ${JSON.stringify(data)}\n\n`);
  }

  ping(tail = 0): void {
    this.named('ping', { tail });
  }

  /** The connection dropped; the browser reconnects by itself with Last-Event-ID. */
  drop(): void {
    this.readyState = FakeEventSource.CONNECTING;
    this.parser.reset();
    this.error();
  }

  /** The browser gave up, as any non-200 answer (Caddy's 502, a 401) makes it. */
  fail(): void {
    this.readyState = FakeEventSource.CLOSED;
    this.error();
  }

  close(): void {
    this.readyState = FakeEventSource.CLOSED;
  }

  private error(): void {
    const event = new Event('error');
    this.onerror?.(event);
    this.dispatchEvent(event);
  }

  private deliver(message: EventSourceMessage): void {
    if (message.id !== undefined) this.lastEventId = message.id;
    const event = new MessageEvent(message.event ?? 'message', {
      data: message.data,
      lastEventId: this.lastEventId,
    });
    if (event.type === 'message') this.onmessage?.(event);
    this.dispatchEvent(event);
  }
}

/** Replaces the global EventSource for one test. */
export function installFakeEventSource(): typeof FakeEventSource {
  FakeEventSource.instances = [];
  vi.stubGlobal('EventSource', FakeEventSource);
  return FakeEventSource;
}
