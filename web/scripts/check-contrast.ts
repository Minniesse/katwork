// Fails when a text/surface token pair in src/theme.css is below WCAG AA (4.5:1) in either
// theme. Run with `npm run check:contrast` (Node 22 strips the types).
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

export type Tokens = Record<string, string>;
export type ThemeName = 'light' | 'dark';
export type Themes = Record<ThemeName, Tokens>;

export interface Failure {
  theme: ThemeName;
  fg: string;
  bg: string;
  ratio: number;
  minimum: number;
}

const HEX = /^#(?:[0-9a-f]{3}|[0-9a-f]{6})$/i;
const DARK = /^(?::root)?\[data-theme=(['"]?)dark\1\]$/;

/** Hex colour tokens from `:root` (light) and `[data-theme=dark]` (dark) rules. */
export function parseThemes(css: string): Themes {
  const themes: Themes = { light: {}, dark: {} };
  const source = css.replace(/\/\*[\s\S]*?\*\//g, '');
  for (const [, selector = '', body = ''] of source.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const trimmed = selector.trim();
    const theme: ThemeName | null =
      trimmed === ':root' ? 'light' : DARK.test(trimmed) ? 'dark' : null;
    if (!theme) continue;
    for (const [, name = '', value = ''] of body.matchAll(/(--[\w-]+)\s*:\s*([^;]+)/g)) {
      if (HEX.test(value.trim())) themes[theme][name] = value.trim().toLowerCase();
    }
  }
  return themes;
}

function channel(value: number): number {
  const s = value / 255;
  return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
}

function luminance(hex: string): number {
  let digits = hex.slice(1);
  if (digits.length === 3) digits = [...digits].map((d) => d + d).join('');
  const [r, g, b] = [0, 2, 4].map((i) => channel(parseInt(digits.slice(i, i + 2), 16)));
  return 0.2126 * r! + 0.7152 * g! + 0.0722 * b!;
}

/** WCAG 2.x contrast ratio, from 1 to 21. */
export function contrastRatio(a: string, b: string): number {
  const [high, low] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (high! + 0.05) / (low! + 0.05);
}

/**
 * The pairs come from token names: `--text*` on `--bg` and every `--surface*`; `--on-X` on
 * `--X`; `--focus*` and `--border-strong` (non-text UI, 3:1) on every surface. A token
 * missing from one theme fails there.
 */
export function findFailures(themes: Themes): Failure[] {
  const names = [...new Set([...Object.keys(themes.light), ...Object.keys(themes.dark)])];
  const surfaces = names.filter((name) => name === '--bg' || name.startsWith('--surface'));
  const pairs: [fg: string, bg: string, minimum: number][] = [];
  for (const fg of names) {
    if (fg.startsWith('--text')) surfaces.forEach((bg) => pairs.push([fg, bg, 4.5]));
    else if (fg.startsWith('--on-')) pairs.push([fg, `--${fg.slice('--on-'.length)}`, 4.5]);
    else if (fg.startsWith('--focus') || fg === '--border-strong')
      surfaces.forEach((bg) => pairs.push([fg, bg, 3]));
  }
  const failures: Failure[] = [];
  for (const theme of ['light', 'dark'] as const) {
    for (const [fg, bg, minimum] of pairs) {
      const a = themes[theme][fg];
      const b = themes[theme][bg];
      const ratio = a && b ? contrastRatio(a, b) : 0;
      if (ratio < minimum) failures.push({ theme, fg, bg, ratio, minimum });
    }
  }
  return failures;
}

function main(): number {
  const css = readFileSync(new URL('../src/theme.css', import.meta.url), 'utf8');
  const themes = parseThemes(css);
  if (Object.keys(themes.light).length === 0 || Object.keys(themes.dark).length === 0) {
    console.error('check:contrast: no colour tokens found in src/theme.css');
    return 1;
  }
  const failures = findFailures(themes);
  for (const { theme, fg, bg, ratio, minimum } of failures) {
    console.error(`${theme}: ${fg} on ${bg} is ${ratio.toFixed(2)}:1, needs ${minimum}:1`);
  }
  if (failures.length > 0) return 1;
  console.log('check:contrast: every text/surface pair passes in light and dark');
  return 0;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main();
}
