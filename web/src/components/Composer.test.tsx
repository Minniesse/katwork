import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, test, vi } from 'vitest';
import en from '../i18n/en.json';
import { Composer } from './Composer';

function setup() {
  const onSend = vi.fn();
  render(<Composer onSend={onSend} />);
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
