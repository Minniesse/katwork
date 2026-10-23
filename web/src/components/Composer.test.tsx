import { act, fireEvent, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, test, vi } from 'vitest';
import { setLocale } from '../i18n';
import en from '../i18n/en.json';
import th from '../i18n/th.json';
import { Composer } from './Composer';

const KATS = [
  { id: 'kat-lead', name: 'หัวหน้าทีม' },
  { id: 'kat-writer', name: 'Writer' },
  { id: 'kat-scheduler', name: 'เลขา' },
];

afterEach(() => act(() => setLocale('en')));

function setup(props: Partial<Parameters<typeof Composer>[0]> = {}) {
  const onSend = vi.fn();
  render(<Composer onSend={onSend} {...props} />);
  const box = screen.getByRole('textbox', { name: en['chat.composer_label'] });
  return { onSend, box };
}

test('composer_enter_during_ime_composition_does_not_send', () => {
  const { onSend, box } = setup();
  fireEvent.change(box, { target: { value: 'สวัสดี' } });

  fireEvent.compositionStart(box);
  fireEvent.keyDown(box, { key: 'Enter', isComposing: true });
  fireEvent.compositionEnd(box);
  // Safari sends the Enter that commits the composition as keyCode 229 after compositionend.
  fireEvent.keyDown(box, { key: 'Enter', keyCode: 229 });
  expect(onSend).not.toHaveBeenCalled();
  expect(box).toHaveValue('สวัสดี');

  fireEvent.keyDown(box, { key: 'Enter' });
  expect(onSend).toHaveBeenCalledExactlyOnceWith('สวัสดี');
  expect(box).toHaveValue('');
});

test('composer_ime_sequence_never_sends_early', async () => {
  const user = userEvent.setup({ delay: null });
  const { onSend, box } = setup();
  await user.type(box, 'ขอบคุณ');

  // Real DOM events, in the order Safari fires them for a Thai or Japanese IME commit.
  fireEvent(box, new CompositionEvent('compositionstart', { bubbles: true }));
  fireEvent(
    box,
    new KeyboardEvent('keydown', {
      key: 'Enter',
      isComposing: true,
      keyCode: 229,
      bubbles: true,
      cancelable: true,
    }),
  );
  fireEvent(box, new CompositionEvent('compositionend', { bubbles: true, data: 'ขอบคุณ' }));
  const commit = new KeyboardEvent('keydown', {
    key: 'Enter',
    isComposing: false,
    keyCode: 229,
    bubbles: true,
    cancelable: true,
  });
  fireEvent(box, commit);
  expect(onSend).not.toHaveBeenCalled();
  // The committing Enter is the IME's, so the page leaves it alone.
  expect(commit.defaultPrevented).toBe(false);

  await user.keyboard('{Shift>}{Enter}{/Shift}ครับ');
  expect(onSend).not.toHaveBeenCalled();
  expect(box).toHaveValue('ขอบคุณ\nครับ');

  const send = new KeyboardEvent('keydown', {
    key: 'Enter',
    keyCode: 13,
    bubbles: true,
    cancelable: true,
  });
  fireEvent(box, send);
  expect(send.defaultPrevented).toBe(true);
  expect(onSend).toHaveBeenCalledExactlyOnceWith('ขอบคุณ\nครับ');
});

test('enter_sends_and_shift_enter_adds_a_line', async () => {
  const user = userEvent.setup({ delay: null });
  const { onSend, box } = setup();
  await user.type(box, 'line one{Shift>}{Enter}{/Shift}line two');
  expect(onSend).not.toHaveBeenCalled();
  await user.keyboard('{Enter}');
  expect(onSend).toHaveBeenCalledExactlyOnceWith('line one\nline two');
});

test('send_button_is_labelled_and_ignores_blank_text', async () => {
  const user = userEvent.setup({ delay: null });
  const { onSend, box } = setup();
  const send = screen.getByRole('button', { name: en['chat.send'] });
  await user.click(send);
  await user.type(box, '   ');
  await user.click(send);
  expect(onSend).not.toHaveBeenCalled();
  await user.type(box, ' hi ');
  await user.click(send);
  expect(onSend).toHaveBeenCalledExactlyOnceWith('hi');
});

test('mention_picker_inserts_a_trailing_space_after_a_thai_name', async () => {
  const user = userEvent.setup({ delay: null });
  const { onSend, box } = setup({ mentions: KATS });
  await user.type(box, '@');
  const list = screen.getByRole('listbox', { name: en['chat.mention_list'] });
  expect(
    within(list)
      .getAllByRole('option')
      .map((option) => option.textContent),
  ).toEqual(['หัวหน้าทีม', 'Writer', 'เลขา']);
  expect(box).toHaveAttribute('aria-controls', list.id);
  expect(box).toHaveAttribute('aria-autocomplete', 'list');

  await user.keyboard('{ArrowDown}{ArrowDown}');
  const active = within(list).getByRole('option', { selected: true });
  expect(active).toHaveTextContent('เลขา');
  expect(box).toHaveAttribute('aria-activedescendant', active.id);

  await user.keyboard('{Enter}');
  // A Thai name followed directly by Thai text would not resolve, so the space is required.
  expect(box).toHaveValue('@เลขา ');
  expect(screen.queryByRole('listbox')).toBeNull();
  expect(onSend).not.toHaveBeenCalled();

  await user.keyboard('ช่วยดูตารางพรุ่งนี้{Enter}');
  expect(onSend).toHaveBeenCalledExactlyOnceWith('@เลขา ช่วยดูตารางพรุ่งนี้');
});

test('mention_picker_filters_by_prefix_and_picks_with_tab_or_click', async () => {
  const user = userEvent.setup({ delay: null });
  const { box } = setup({ mentions: KATS });
  await user.type(box, 'Please ask @wr');
  expect(screen.getAllByRole('option').map((option) => option.textContent)).toEqual(['Writer']);
  await user.keyboard('{Tab}');
  expect(box).toHaveValue('Please ask @Writer ');
  expect(box).toHaveFocus();

  await user.type(box, 'and @หัว');
  await user.click(screen.getByRole('option', { name: 'หัวหน้าทีม' }));
  expect(box).toHaveValue('Please ask @Writer and @หัวหน้าทีม ');
  expect(box).toHaveFocus();
});

test('mention_picker_closes_on_escape_and_ignores_email_at_signs', async () => {
  const user = userEvent.setup({ delay: null });
  const { onSend, box } = setup({ mentions: KATS });
  await user.type(box, 'mail bob@');
  expect(screen.queryByRole('listbox')).toBeNull();

  await user.clear(box);
  await user.type(box, 'hi @');
  expect(screen.getByRole('listbox')).toBeInTheDocument();
  await user.keyboard('{Escape}');
  expect(screen.queryByRole('listbox')).toBeNull();
  await user.keyboard('{Enter}');
  expect(onSend).toHaveBeenCalledExactlyOnceWith('hi @');
});

test('mention_picker_is_localized', async () => {
  act(() => setLocale('th'));
  const user = userEvent.setup({ delay: null });
  render(<Composer onSend={vi.fn()} mentions={KATS} />);
  await user.type(screen.getByRole('textbox', { name: th['chat.composer_label'] }), '@');
  expect(screen.getByRole('listbox', { name: th['chat.mention_list'] })).toBeInTheDocument();
});

test('mention_picker_announces_the_current_choice', async () => {
  const user = userEvent.setup({ delay: null });
  const { box } = setup({ mentions: KATS });
  const status = () => screen.getByRole('status');
  expect(status()).toHaveTextContent('');
  // A textarea may not carry aria-expanded; the picker is announced instead.
  expect(box).not.toHaveAttribute('aria-expanded');
  await user.type(box, '@');
  expect(status()).toHaveTextContent('หัวหน้าทีม, 1 of 3. Enter inserts the name.');
  await user.keyboard('{ArrowDown}');
  expect(status()).toHaveTextContent('Writer, 2 of 3. Enter inserts the name.');
  await user.keyboard('{Escape}');
  expect(status()).toHaveTextContent('');
});
