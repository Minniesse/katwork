import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, test } from 'vitest';
import { contrastRatio, findFailures, parseThemes } from './check-contrast.ts';

test('contrast_ratio_matches_wcag_reference_values', () => {
  expect(contrastRatio('#000000', '#ffffff')).toBeCloseTo(21, 5);
  expect(contrastRatio('#ffffff', '#ffffff')).toBeCloseTo(1, 5);
  expect(contrastRatio('#777777', '#ffffff')).toBeCloseTo(4.48, 2);
  expect(contrastRatio('#fff', '#000')).toBeCloseTo(21, 5);
});

test('parses_light_and_dark_token_blocks', () => {
  const themes = parseThemes(`
    :root { --bg: #ffffff; --text: #111111; --radius: 8px; }
    [data-theme='dark'] { --bg: #000000; --text: #eeeeee; }
  `);
  expect(themes.light).toEqual({ '--bg': '#ffffff', '--text': '#111111' });
  expect(themes.dark).toEqual({ '--bg': '#000000', '--text': '#eeeeee' });
});

test('flags_a_text_and_surface_pair_below_4_5', () => {
  const failures = findFailures(
    parseThemes(`
      :root { --bg: #ffffff; --surface: #ffffff; --text: #111111; --text-muted: #8a8a8a;
              --primary: #2453c9; --on-primary: #ffffff; }
      [data-theme=dark] { --bg: #000000; --surface: #000000; --text: #eeeeee; --text-muted: #cccccc;
              --primary: #2453c9; --on-primary: #ffffff; }
    `),
  );
  expect(failures).toEqual([
    expect.objectContaining({ theme: 'light', fg: '--text-muted', bg: '--bg' }),
    expect.objectContaining({ theme: 'light', fg: '--text-muted', bg: '--surface' }),
  ]);
});

test('a_theme_missing_a_token_fails', () => {
  const failures = findFailures(
    parseThemes(`:root { --bg: #fff; --text: #000; } [data-theme=dark] { --bg: #000; }`),
  );
  expect(failures).toEqual([expect.objectContaining({ theme: 'dark', fg: '--text', bg: '--bg' })]);
});

test('shipped_theme_passes', () => {
  // Vitest stubs CSS imports, `?raw` included, so read the file itself.
  const css = readFileSync(join(import.meta.dirname, '../src/theme.css'), 'utf8');
  const themes = parseThemes(css);
  expect(Object.keys(themes.light).length).toBeGreaterThan(5);
  expect(findFailures(themes)).toEqual([]);
});
