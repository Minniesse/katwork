import { afterEach, expect, test, vi } from 'vitest';
import html from '../index.html?raw';
import script from '../public/theme-init.js?raw';

// The pre-paint script runs from index.html before React loads; the CSP forbids inline scripts.

function prefersDark(dark: boolean) {
  vi.stubGlobal(
    'matchMedia',
    vi.fn((query: string) => ({ matches: dark && query.includes('dark'), media: query })),
  );
}

function runPrePaint() {
  delete document.documentElement.dataset.theme;
  document.documentElement.lang = '';
  new Function(script)();
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

test('pre_paint_script_is_external_and_runs_before_the_app', () => {
  const head = html.slice(0, html.indexOf('</head>'));
  const themeTag = head.indexOf('<script src="/theme-init.js"></script>');
  expect(themeTag).toBeGreaterThan(-1);
  expect(head.indexOf('type="module"')).toBe(-1);
  expect(html).not.toMatch(/<script>(?!<\/script>)/);
});

test('pre_paint_uses_the_saved_theme_and_locale', () => {
  prefersDark(false);
  localStorage.setItem('kw-theme', 'dark');
  localStorage.setItem('kw-locale', 'th');
  runPrePaint();
  expect(document.documentElement.dataset.theme).toBe('dark');
  expect(document.documentElement.lang).toBe('th');
});

test('pre_paint_follows_the_system_without_a_saved_theme', () => {
  prefersDark(true);
  runPrePaint();
  expect(document.documentElement.dataset.theme).toBe('dark');
  prefersDark(false);
  runPrePaint();
  expect(document.documentElement.dataset.theme).toBe('light');
});

test('pre_paint_survives_blocked_storage', () => {
  prefersDark(true);
  vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
    throw new DOMException('denied', 'SecurityError');
  });
  runPrePaint();
  expect(document.documentElement.dataset.theme).toBe('dark');
});

test('pre_paint_ignores_unknown_saved_values', () => {
  prefersDark(false);
  localStorage.setItem('kw-theme', 'purple');
  localStorage.setItem('kw-locale', 'xx');
  runPrePaint();
  expect(document.documentElement.dataset.theme).toBe('light');
  expect(document.documentElement.lang).not.toBe('xx');
});
