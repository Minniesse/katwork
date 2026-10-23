import { useId, useRef, useState, type FormEvent, type KeyboardEvent } from 'react';
import { t, useLocale } from '../i18n';

interface Props {
  onSend: (text: string) => void;
  disabled?: boolean;
}

/** Enter sends, Shift+Enter adds a line, and an Enter that confirms an IME candidate never sends. */
export function Composer({ onSend, disabled = false }: Props) {
  useLocale();
  const [text, setText] = useState('');
  const composing = useRef(false);
  const id = useId();

  const send = () => {
    const value = text.trim();
    if (!value || disabled) return;
    onSend(value);
    setText('');
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key !== 'Enter' || event.shiftKey) return;
    // Safari fires the committing Enter after compositionend, flagged only by keyCode 229.
    if (composing.current || event.nativeEvent.isComposing || event.keyCode === 229) return;
    event.preventDefault();
    send();
  };

  const onSubmit = (event: FormEvent) => {
    event.preventDefault();
    send();
  };

  return (
    <form className="composer" onSubmit={onSubmit}>
      <label className="composer-label" htmlFor={`${id}-text`}>
        {t('chat.composer_label')}
      </label>
      <div className="composer-row">
        <textarea
          id={`${id}-text`}
          className="composer-input"
          rows={2}
          value={text}
          aria-describedby={`${id}-hint`}
          onChange={(event) => setText(event.target.value)}
          onKeyDown={onKeyDown}
          onCompositionStart={() => {
            composing.current = true;
          }}
          onCompositionEnd={() => {
            composing.current = false;
          }}
        />
        <button type="submit" className="button button-primary" disabled={disabled}>
          {t('chat.send')}
        </button>
      </div>
      <p id={`${id}-hint`} className="hint">
        {t('chat.composer_hint')}
      </p>
    </form>
  );
}
