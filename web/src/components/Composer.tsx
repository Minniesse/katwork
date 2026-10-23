import {
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent,
} from 'react';
import { t, useLocale } from '../i18n';

export interface Mentionable {
  id: string;
  name: string;
}

interface Props {
  onSend: (text: string) => void;
  disabled?: boolean;
  /** The Kats the `@` picker offers, in team order. */
  mentions?: Mentionable[];
}

// An `@` at the start or after whitespace, then the name typed so far, right before the caret.
// Only the picker reads this: the server's `mentions::resolve` decides who a message names.
const TRIGGER = /(?:^|\s)@([^\s@]*)$/u;

function trigger(text: string, caret: number): { start: number; query: string } | null {
  const match = TRIGGER.exec(text.slice(0, caret));
  if (!match) return null;
  return { start: caret - match[1]!.length - 1, query: match[1]! };
}

const fold = (text: string) => text.normalize('NFKC').toLocaleLowerCase();

/**
 * Enter sends, Shift+Enter adds a line, and an Enter that confirms an IME candidate never
 * sends. Typing `@` offers the team's Kats; choosing one inserts the name and a space, because
 * a Thai name followed directly by Thai text would not resolve as a mention.
 */
export function Composer({ onSend, disabled = false, mentions = [] }: Props) {
  useLocale();
  const [text, setText] = useState('');
  const [caret, setCaret] = useState(0);
  const [active, setActive] = useState(0);
  const [dismissed, setDismissed] = useState<number | null>(null);
  const composing = useRef(false);
  const box = useRef<HTMLTextAreaElement>(null);
  const nextCaret = useRef<number | null>(null);
  const id = useId();

  const typed = trigger(text, caret);
  const options =
    typed && dismissed !== typed.start
      ? mentions.filter((kat) => fold(kat.name).startsWith(fold(typed.query)))
      : [];
  const open = options.length > 0;
  const current = Math.min(active, options.length - 1);
  const optionId = (index: number) => `${id}-mention-${index}`;

  useLayoutEffect(() => {
    if (nextCaret.current === null || !box.current) return;
    box.current.setSelectionRange(nextCaret.current, nextCaret.current);
    nextCaret.current = null;
  });

  const update = (value: string, position: number) => {
    setText(value);
    setCaret(position);
    setActive(0);
  };

  const pick = (kat: Mentionable) => {
    if (!typed) return;
    const inserted = `@${kat.name} `;
    const value = text.slice(0, typed.start) + inserted + text.slice(caret);
    const position = typed.start + inserted.length;
    nextCaret.current = position;
    update(value, position);
    box.current?.focus();
  };

  const send = () => {
    const value = text.trim();
    if (!value || disabled) return;
    onSend(value);
    update('', 0);
    setDismissed(null);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    // Safari fires the committing Enter after compositionend, flagged only by keyCode 229.
    if (composing.current || event.nativeEvent.isComposing || event.keyCode === 229) return;
    if (open) {
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
        event.preventDefault();
        const step = event.key === 'ArrowDown' ? 1 : -1;
        setActive((current + step + options.length) % options.length);
        return;
      }
      if ((event.key === 'Enter' && !event.shiftKey) || event.key === 'Tab') {
        event.preventDefault();
        pick(options[current]!);
        return;
      }
      if (event.key === 'Escape') {
        event.preventDefault();
        setDismissed(typed!.start);
        return;
      }
    }
    if (event.key !== 'Enter' || event.shiftKey) return;
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
        <div className="composer-field">
          <textarea
            ref={box}
            id={`${id}-text`}
            className="composer-input"
            rows={2}
            value={text}
            aria-describedby={`${id}-hint`}
            aria-autocomplete={mentions.length > 0 ? 'list' : undefined}
            aria-controls={open ? `${id}-mentions` : undefined}
            aria-activedescendant={open ? optionId(current) : undefined}
            onChange={(event) => update(event.target.value, event.target.selectionStart)}
            onSelect={(event) => setCaret(event.currentTarget.selectionStart)}
            onKeyDown={onKeyDown}
            onCompositionStart={() => {
              composing.current = true;
            }}
            onCompositionEnd={() => {
              composing.current = false;
            }}
          />
          {open && (
            <ul
              id={`${id}-mentions`}
              className="mention-list"
              role="listbox"
              aria-label={t('chat.mention_list')}
            >
              {options.map((kat, index) => (
                <li
                  key={kat.id}
                  id={optionId(index)}
                  role="option"
                  aria-selected={index === current}
                  className="mention-option"
                  // Keeps focus in the text box, so typing goes on after the pick.
                  onMouseDown={(event) => event.preventDefault()}
                  onClick={() => pick(kat)}
                >
                  {kat.name}
                </li>
              ))}
            </ul>
          )}
        </div>
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
