import { useEffect, useLayoutEffect, useRef } from 'react';
import { StickToBottom, useStickToBottomContext } from 'use-stick-to-bottom';
import { formatTime, hasMessage, t, useLocale } from '../i18n';
import type { ChatMessage } from '../sse';

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
}

/**
 * The conversation. The `log` region holds stored messages only: text still streaming sits in
 * the same list, in id order, but hidden from assistive tech, and the stored message replaces
 * it as a new node, so a screen reader hears each finished message once. Scrolling follows new
 * content until the person scrolls up.
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
}: Props) {
  useLocale();
  const empty = messages.length === 0 && pending.length === 0;
  return (
    <StickToBottom className="transcript" initial="instant" resize="smooth">
      <StickToBottom.Content className="transcript-content">
        <EarlierMessages earlier={earlier} onEarlier={onEarlier} count={messages.length} />
        <div
          role="log"
          aria-live="polite"
          aria-relevant="additions"
          aria-label={t('chat.transcript')}
        >
          {empty ? (
            <p className="transcript-empty">{t('chat.empty')}</p>
          ) : (
            <ol className="transcript-list">
              {messages.map((message) => (
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
              ))}
              {pending.map((post) => (
                <PendingItem key={post.clientMsgId} post={post} onRetry={onRetry} />
              ))}
            </ol>
          )}
        </div>
      </StickToBottom.Content>
      <JumpToLatest />
    </StickToBottom>
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
  return hasMessage(key) ? t(key) : message.body;
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
      <p className="message-body">{kind === 'system' ? noticeText(message) : message.body}</p>
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
