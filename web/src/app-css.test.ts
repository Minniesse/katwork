// jsdom has no layout, so the phone header's wrapping is pinned on the stylesheet itself; the
// PR records the headless-Chrome check at 400 px with one picker and with two.
import { readFileSync } from 'node:fs';
import { expect, test } from 'vitest';

const css = readFileSync('src/app.css', 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');

/** The declarations of `selector` inside the `@media (max-width: 40rem)` block. */
function phoneRule(selector: string): string {
  const start = css.indexOf('@media (max-width: 40rem)');
  expect(start, 'phone media block').toBeGreaterThan(-1);
  const block = css.slice(start);
  const rule = new RegExp(`(?:^|[},])\\s*${selector.replace('.', '\\.')}\\s*\\{([^}]*)\\}`, 'm');
  return rule.exec(block)?.[1] ?? '';
}

test('phone_header_wraps_when_both_pickers_show', () => {
  // Two workspaces and two Spaces don't fit beside the four links at 400 px.
  for (const selector of ['.shell-nav', '.pickers']) {
    expect(phoneRule(selector), selector).not.toMatch(/flex-wrap:\s*nowrap/);
  }
  expect(phoneRule('.shell-nav')).toMatch(/flex-wrap:\s*wrap/);
  expect(phoneRule('.pickers')).toMatch(/flex-wrap:\s*wrap/);
  // The single-picker case keeps the links on one compact row.
  expect(phoneRule('.nav-links')).toMatch(/flex-wrap:\s*nowrap/);
});
