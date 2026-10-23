import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { StickToBottom, useStickToBottomContext } from 'use-stick-to-bottom';
import { formatTime, hasMessage, t, tPlural, useLocale } from '../i18n';
import type { ChatMessage } from '../sse';

// The new-arrival announcer. More than BURST arrivals with no quiet gap of QUIET_MS between
// them are summed up in one line once things are quiet; each line leaves the log CLEAR_MS after
// it was added (removals are silent), and the log never holds more than MAX_LINES.
const BURST = 3;
const QUIET_MS = 1_000;
const CLEAR_MS = 5_000;
const MAX_LINES = 5;

/** The person's own post before the server confirmed it, keyed by its `client_msg_id`. */
export interface PendingMessage {
  clientMsgId: string;
  body: string;
  /** Set when the post failed; the person can send it again with the same `client_msg_id`. */
  error: string | null;
}

/**
 * Older messages: `more` offers "Show earlier messages", `loading` is fetching them, `start`
 * means the person paged back to the first message, `none` that the first page held them all.
 */
export type Earlier = 'none' | 'more' | 'loading' | 'start';

interface Props {
  messages: ChatMessage[];
  pending?: PendingMessage[];
  /** The signed-in user, whose messages read "You". */
  me?: string | null;
  /** A Kat's display name from the team, for text still streaming. */
  katName?: (katId: string) => string | undefined;
  /** The question whose options can be clicked now; every other question is read-only. */
  activeQuestion?: string | null;
  optionsDisabled?: boolean;
  onOption?: (option: string) => void;
  onRetry?: (clientMsgId: string) => void;
  earlier?: Earlier;
  onEarlier?: () => void | Promise<void>;
  /**
   * Ids that came from history (the page's first load and "Show earlier messages"): shown in
   * the list but never announced. Defaults to the ids of the first render.
   */
  history?: ReadonlySet<string>;
  /** How many messages the last older page brought, announced once per page. */
  earlierLoaded?: number | null;
}

/**
 * The conversation: one list in message-id order, history and live arrivals alike, so a reply
 * stored late still sits between its neighbours. The list itself is not a live region. A
 * separate, visually hidden `log` announces each genuinely new stored message once ("author:
 * text"), and never history, so loading older pages reads nothing out but one count line.
 * Text still streaming is hidden from assistive tech. Scrolling follows new content until the
 * person scrolls up.
 */
export function Transcript({
  messages,
  pending = [],
  me = null,
  katName,
  activeQuestion = null,
  optionsDisabled = false,
  onOption,
  onRetry,
  earlier = 'none',
  onEarlier,
  history,
  earlierLoaded = null,
}: Props) {
  useLocale();
  const firstRender = useRef<ReadonlySet<string> | null>(null);
  firstRender.current ??= new Set(messages.map((message) => message.id));
  const known = history ?? firstRender.current;
  const empty = messages.length === 0 && pending.length === 0;
  const item = (message: ChatMessage) => (
    <MessageItem
      key={`${message.id}:${message.provisional ? 'streaming' : 'stored'}`}
      message={message}
      me={me}
      katName={katName}
      options={
        message.id === activeQuestion
          ? { disabled: optionsDisabled, onOption: onOption ?? (() => {}) }
          : null
      }
    />
  );
  return (
    <StickToBottom className="transcript" initial="instant" resize="smooth">
      <StickToBottom.Content className="transcript-content">
        <EarlierMessages earlier={earlier} onEarlier={onEarlier} count={messages.length} />
        <p className="sr-only" role="status" data-testid="earlier-status">
          {earlierLoaded !== null &&
            earlierLoaded > 0 &&
            tPlural('chat.earlier_loaded', earlierLoaded)}
        </p>
        {empty ? (
          <p className="transcript-empty">{t('chat.empty')}</p>
        ) : (
          <ol className="transcript-list" aria-label={t('chat.transcript')}>
            {messages.map(item)}
            {pending.map((post) => (
              <PendingItem key={post.clientMsgId} post={post} onRetry={onRetry} />
            ))}
          </ol>
        )}
        <NewArrivals
          messages={messages}
          known={known}
          describe={(message) => `${authorName(message, me, katName)}: ${messageText(message)}`}
        />
      </StickToBottom.Content>
      <JumpToLatest />
    </StickToBottom>
  );
}

type Line = { key: string; message: ChatMessage } | { key: string; count: number };

/**
 * A visually hidden log that announces each genuinely new stored message once, in the order
 * it arrives (not its id order: a reply minted at its run's start can be stored after many
 * newer messages). Ids from history (`known`) are marked seen and never announced, and an id
 * is never announced twice, whatever the history set later holds. A rush of arrivals is summed
 * up as "N new messages", and every line leaves shortly after it is added, so the log is
 * normally empty and a browsing screen reader doesn't meet a message twice.
 */
function NewArrivals({
  messages,
  known,
  describe,
}: {
  messages: ChatMessage[];
  known: ReadonlySet<string>;
  describe: (message: ChatMessage) => string;
}) {
  useLocale();
  const [lines, setLines] = useState<Line[]>([]);
  const seen = useRef(new Set<string>());
  const rush = useRef({ last: -Infinity, count: 0, pending: 0 });
  const timers = useRef(new Set<ReturnType<typeof setTimeout>>());
  const summary = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const keys = useRef(0);

  useEffect(() => {
    const pending = timers.current;
    return () => {
      pending.forEach(clearTimeout);
      pending.clear();
      clearTimeout(summary.current);
    };
  }, []);

  useEffect(() => {
    const add = (line: Line) => {
      setLines((current) => [...current, line].slice(-MAX_LINES));
      const timer = setTimeout(() => {
        timers.current.delete(timer);
        setLines((current) => current.filter((other) => other.key !== line.key));
      }, CLEAR_MS);
      timers.current.add(timer);
    };
    for (const id of known) seen.current.add(id);
    const fresh = messages.filter(
      (message) => !message.provisional && !seen.current.has(message.id),
    );
    if (fresh.length === 0) return;
    for (const message of fresh) seen.current.add(message.id);
    const now = Date.now();
    const state = rush.current;
    if (now - state.last > QUIET_MS) state.count = 0;
    state.last = now;
    state.count += fresh.length;
    if (state.pending === 0 && state.count <= BURST) {
      fresh.forEach((message) => add({ key: `m:${message.id}`, message }));
      return;
    }
    state.pending += fresh.length;
    clearTimeout(summary.current);
    summary.current = setTimeout(() => {
      keys.current += 1;
      add({ key: `n:${keys.current}`, count: state.pending });
      state.pending = 0;
    }, QUIET_MS);
  });

  return (
    <div
      className="sr-only"
      role="log"
      aria-live="polite"
      aria-relevant="additions"
      aria-label={t('chat.new_messages')}
    >
      {lines.map((line) => (
        <p key={line.key}>
          {'count' in line
            ? tPlural('chat.new_messages_count', line.count)
            : describe(line.message)}
        </p>
      ))}
    </div>
  );
}

/**
 * "Show earlier messages". Older messages land above what the person is reading, so the
 * distance from the bottom is kept across the load and the same message stays in view.
 */
function EarlierMessages({
  earlier,
  onEarlier,
  count,
}: {
  earlier: Earlier;
  onEarlier: Props['onEarlier'];
  count: number;
}) {
  const { scrollRef } = useStickToBottomContext();
  const fromBottom = useRef<number | null>(null);
  const start = useRef<HTMLParagraphElement>(null);
  const focusStart = useRef(false);

  useLayoutEffect(() => {
    const scroller = scrollRef.current;
    if (fromBottom.current === null || !scroller) return;
    scroller.scrollTop = scroller.scrollHeight - fromBottom.current;
    fromBottom.current = null;
  }, [count, scrollRef]);
  useEffect(() => {
    // The button the person pressed is gone; focus goes to the line that replaced it.
    if (earlier !== 'start' || !focusStart.current) return;
    focusStart.current = false;
    start.current?.focus();
  }, [earlier]);

  if (earlier === 'none') return null;
  if (earlier === 'start') {
    return (
      <p ref={start} tabIndex={-1} className="transcript-start">
        {t('chat.start')}
      </p>
    );
  }
  const loading = earlier === 'loading';
  const load = () => {
    if (loading) return;
    const scroller = scrollRef.current;
    fromBottom.current = scroller ? scroller.scrollHeight - scroller.scrollTop : null;
    focusStart.current = true;
    void onEarlier?.();
  };
  return (
    // aria-disabled, not disabled, so focus stays on the button while it loads.
    <button
      type="button"
      className="button button-quiet transcript-earlier"
      aria-disabled={loading || undefined}
      onClick={load}
    >
      {loading ? t('chat.earlier_loading') : t('chat.earlier')}
    </button>
  );
}

function JumpToLatest() {
  const { isAtBottom, scrollToBottom } = useStickToBottomContext();
  if (isAtBottom) return null;
  return (
    <button type="button" className="button jump-latest" onClick={() => void scrollToBottom()}>
      {t('chat.jump_latest')}
    </button>
  );
}

function authorName(message: ChatMessage, me: string | null, katName: Props['katName']): string {
  switch (message.author?.kind) {
    case 'user':
      if (message.author.id === me) return t('chat.author.you');
      return message.authorName || t('chat.author.member');
    case 'system':
      return t('chat.author.system');
    case 'kat':
      return message.authorName || katName?.(message.author.id) || t('chat.author.kat');
    default:
      return t('chat.author.kat');
  }
}

/** A notice's copy comes from its own code (Task 6's `NoticeMeta`), never from its job's reason. */
function noticeText(message: ChatMessage): string {
  const code = message.meta?.code ?? message.body;
  const key = `reason.${code}`;
  // Some notices name their Kats in the sentence (Task 12's `mention_needs_space`).
  const name = message.meta?.kats.map((kat) => kat.name).join(', ') ?? '';
  return hasMessage(key) ? t(key, { name }) : message.body;
}

function messageText(message: ChatMessage): string {
  return message.author?.kind === 'system' ? noticeText(message) : message.body;
}

function MessageItem({
  message,
  me,
  katName,
  options,
}: {
  message: ChatMessage;
  me: string | null;
  katName: Props['katName'];
  options: { disabled: boolean; onOption: (option: string) => void } | null;
}) {
  const kind = message.author?.kind ?? 'kat';
  const kats = message.meta?.kats ?? [];
  return (
    <li
      className={`message message-${kind}`}
      data-provisional={message.provisional || undefined}
      aria-hidden={message.provisional || undefined}
    >
      <p className="message-meta">
        <span className="message-author">{authorName(message, me, katName)}</span>
        {message.synthetic && (
          <span className="badge badge-demo" title={t('chat.synthetic_hint')}>
            {t('chat.synthetic_badge')}
          </span>
        )}
        {message.stopped && <span className="badge badge-stopped">{t('chat.stopped_label')}</span>}
        {message.provisional && <span className="message-writing">{t('chat.writing')}</span>}
        {message.createdAt && (
          <time className="message-time" dateTime={message.createdAt}>
            {formatTime(message.createdAt)}
          </time>
        )}
      </p>
      <p className="message-body">{messageText(message)}</p>
      {kind === 'system' && kats.length > 0 && (
        <p className="message-about">
          {t('chat.notice_about', { names: kats.map((kat) => kat.name).join(', ') })}
        </p>
      )}
      {message.question && message.question.options.length > 0 && (
        <Options question={message.question.options} active={options} />
      )}
    </li>
  );
}

function Options({
  question,
  active,
}: {
  question: string[];
  active: { disabled: boolean; onOption: (option: string) => void } | null;
}) {
  if (!active) {
    return (
      <ul className="message-options" aria-label={t('chat.question_options')}>
        {question.map((option) => (
          <li key={option}>{option}</li>
        ))}
      </ul>
    );
  }
  return (
    <div className="message-options" role="group" aria-label={t('chat.question_options')}>
      {question.map((option) => (
        <button
          key={option}
          type="button"
          className="button option-button"
          disabled={active.disabled}
          onClick={() => active.onOption(option)}
        >
          {option}
        </button>
      ))}
    </div>
  );
}

function PendingItem({ post, onRetry }: { post: PendingMessage; onRetry: Props['onRetry'] }) {
  const failed = post.error !== null;
  return (
    <li className="message message-user" data-pending aria-hidden={failed ? undefined : true}>
      <p className="message-meta">
        <span className="message-author">{t('chat.author.you')}</span>
        {!failed && <span className="message-writing">{t('chat.sending')}</span>}
      </p>
      <p className="message-body">{post.body}</p>
      {failed && (
        <div className="message-failed" role="alert">
          <p>
            {t('chat.not_sent')} {post.error}
          </p>
          <button
            type="button"
            className="button button-quiet"
            onClick={() => onRetry?.(post.clientMsgId)}
          >
            {t('chat.retry')}
          </button>
        </div>
      )}
    </li>
  );
}
