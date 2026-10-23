import { useSyncExternalStore } from 'react';
import en from './en.json';
import th from './th.json';

export type Locale = 'en' | 'th';
export type Calendar = 'gregory' | 'buddhist';
export type Vars = Record<string, string | number>;

export const LOCALES: readonly Locale[] = ['en', 'th'];

const catalogues: Record<Locale, Record<string, string>> = { en, th };
const STORAGE_KEY = 'kw-locale';
const listeners = new Set<() => void>();

export function isLocale(value: unknown): value is Locale {
  return value === 'en' || value === 'th';
}

/** The saved locale, else the browser's language. Storage can throw (privacy modes, blocked cookies). */
export function initialLocale(): Locale {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (isLocale(saved)) return saved;
  } catch {
    // Storage is unavailable; fall through to the browser language.
  }
  return navigator.language.toLowerCase().startsWith('th') ? 'th' : 'en';
}

let current: Locale = initialLocale();

export function getLocale(): Locale {
  return current;
}

export function setLocale(locale: Locale): void {
  current = locale;
  try {
    localStorage.setItem(STORAGE_KEY, locale);
  } catch {
    // The switch still applies for this page; it just isn't remembered.
  }
  document.documentElement.lang = locale;
  listeners.forEach((listener) => listener());
}

function lookup(locale: Locale, key: string): string | undefined {
  const catalogue = catalogues[locale];
  return Object.hasOwn(catalogue, key) ? catalogue[key] : undefined;
}

export function hasMessage(key: string): boolean {
  return lookup('en', key) !== undefined;
}

/** The message for `key` in the current locale, with `{name}` placeholders filled from `vars`. */
export function t(key: string, vars?: Vars): string {
  const template = lookup(current, key) ?? lookup('en', key) ?? key;
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (match, name: string) =>
    Object.hasOwn(vars, name) ? String(vars[name]) : match,
  );
}

/**
 * A message that depends on a count: `{key}.{plural category}` (`one`, `other`, …) for the
 * current locale, falling back to `{key}.other`, with `{count}` filled. English says "1 earlier
 * message", Thai has one form.
 */
export function tPlural(key: string, count: number, vars?: Vars): string {
  const category = new Intl.PluralRules(current === 'th' ? 'th-TH' : 'en-GB').select(count);
  const variant = `${key}.${category}`;
  return t(hasMessage(variant) ? variant : `${key}.other`, { ...vars, count });
}

/** Gregorian by default (UI chrome); pass `calendar: 'buddhist'` where a template asks for the Buddhist Era. */
export function formatDate(
  date: Date | string | number,
  options: { calendar?: Calendar; timeZone?: string } = {},
): string {
  const tag = current === 'th' ? 'th-TH' : 'en-GB';
  return new Intl.DateTimeFormat(`${tag}-u-ca-${options.calendar ?? 'gregory'}`, {
    dateStyle: 'long',
    timeZone: options.timeZone,
  }).format(new Date(date));
}

/** A time of day for the UI chrome, such as when a message was sent. */
export function formatTime(date: Date | string | number, options: { timeZone?: string } = {}): string {
  const tag = current === 'th' ? 'th-TH' : 'en-GB';
  return new Intl.DateTimeFormat(`${tag}-u-ca-gregory`, {
    timeStyle: 'short',
    timeZone: options.timeZone,
  }).format(new Date(date));
}

/** A date and time for the UI chrome, such as when an untitled conversation started. */
export function formatDateTime(
  date: Date | string | number,
  options: { timeZone?: string } = {},
): string {
  const tag = current === 'th' ? 'th-TH' : 'en-GB';
  return new Intl.DateTimeFormat(`${tag}-u-ca-gregory`, {
    dateStyle: 'medium',
    timeStyle: 'short',
    timeZone: options.timeZone,
  }).format(new Date(date));
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Re-renders the calling component when the locale changes. */
export function useLocale(): Locale {
  return useSyncExternalStore(subscribe, getLocale);
}
