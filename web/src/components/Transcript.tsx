import { hasMessage, t, useLocale } from '../i18n';
import type { ChatMessage } from '../sse';

interface Props {
  messages: ChatMessage[];
  /** A Dot's display name; the Team data arrives in 13b. */
  dotName?: (dotId: string) => string | undefined;
}

/**
 * The conversation as a polite live region. While a reply streams, `aria-busy` holds the
 * announcement until the text settles, so a screen reader doesn't read every delta.
 */
export function Transcript({ messages, dotName }: Props) {
  useLocale();
  const streaming = messages.some((message) => message.provisional);
  return (
    <div
      className="transcript"
      role="log"
      aria-live="polite"
      aria-relevant="additions"
      aria-busy={streaming}
      aria-label={t('chat.transcript')}
    >
      {messages.length === 0 ? (
        <p className="transcript-empty">{t('chat.empty')}</p>
      ) : (
        <ol className="transcript-list">
          {messages.map((message) => (
            <MessageItem key={message.id} message={message} dotName={dotName} />
          ))}
        </ol>
      )}
    </div>
  );
}

function authorName(message: ChatMessage, dotName: Props['dotName']): string {
  switch (message.author?.kind) {
    case 'user':
      // TODO(13b): show other members' names once the workspace member list exists.
      return t('chat.author.you');
    case 'system':
      return t('chat.author.system');
    case 'dot':
      return dotName?.(message.author.id) ?? t('chat.author.dot');
    default:
      return t('chat.author.dot');
  }
}

function MessageItem({ message, dotName }: { message: ChatMessage; dotName: Props['dotName'] }) {
  const kind = message.author?.kind ?? 'dot';
  // A system notice's body is a reason code (plan Task 10).
  const noticeKey = `reason.${message.body}`;
  const body = kind === 'system' && hasMessage(noticeKey) ? t(noticeKey) : message.body;
  return (
    <li className={`message message-${kind}`} data-provisional={message.provisional || undefined}>
      <p className="message-meta">
        <span className="message-author">{authorName(message, dotName)}</span>
        {message.synthetic && (
          <span className="badge badge-demo" title={t('chat.synthetic_hint')}>
            {t('chat.synthetic_badge')}
          </span>
        )}
        {message.provisional && <span className="message-writing">{t('chat.writing')}</span>}
      </p>
      <p className="message-body">{body}</p>
      {message.question && message.question.options.length > 0 && (
        <ul className="message-options" aria-label={t('chat.question_options')}>
          {message.question.options.map((option) => (
            <li key={option}>{option}</li>
          ))}
        </ul>
      )}
    </li>
  );
}
