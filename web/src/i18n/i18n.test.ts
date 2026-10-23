import { afterEach, describe, expect, test, vi } from 'vitest';
import en from './en.json';
import th from './th.json';
import { formatDate, getLocale, initialLocale, setLocale, t } from './index';

// Mirrors katwork::types::ReasonCode::ALL; the Rust test in
// crates/katwork-server/tests/i18n.rs checks the same keys against the enum itself.
const REASON_CODES = [
  'provider_unavailable',
  'provider_rate_limited',
  'provider_quota',
  'provider_auth_failed',
  'model_protocol_error',
  'time_limit',
  'quota_exceeded',
  'workspace_paused',
  'step_limit',
  'token_budget',
  'output_truncated',
  'residency_denied',
  'loop_detected',
  'policy_changed',
  'approval_pending',
  'question_pending',
  'interrupted_review_required',
  'kat_paused',
  'retries_exhausted',
  'cancelled',
  'outcome_unknown',
  'no_eligible_kat',
  'kat_not_in_space',
  'lease_lost',
  'too_many_requests',
  'workspace_paused_stopped',
  'kat_paused_stopped',
];

// API error codes the auth routes and guards can return (KatError::code).
const API_CODES = [
  'unauthenticated',
  'reset_token_invalid',
  'password_invalid',
  'rate_limited',
  'request_invalid',
  'body_too_large',
  'auth_busy',
  'auth_failed',
  'mail_delivery_failed',
  'host_not_allowed',
  'cross_site_request',
  'json_required',
  'database_unavailable',
  'database_error',
  'not_found',
  'network_error',
  'unknown',
];

afterEach(() => {
  setLocale('en');
  vi.restoreAllMocks();
});

describe('catalogues', () => {
  test('every_key_exists_in_both_locales', () => {
    expect(Object.keys(th).sort()).toEqual(Object.keys(en).sort());
    for (const catalogue of [en, th]) {
      for (const [key, value] of Object.entries(catalogue)) {
        expect(value.trim(), key).not.toBe('');
      }
    }
  });

  test('placeholders_match_between_locales', () => {
    const vars = (s: string) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
    for (const [key, value] of Object.entries(en)) {
      expect(vars((th as Record<string, string>)[key]!), key).toEqual(vars(value));
    }
  });

  test('every_reason_code_has_a_message_in_both_locales', () => {
    for (const code of [...REASON_CODES, ...API_CODES]) {
      expect(en, code).toHaveProperty([`reason.${code}`]);
      expect(th, code).toHaveProperty([`reason.${code}`]);
    }
  });

  test('copy_calls_coworkers_kats_and_thai_never_says_ai_employee', () => {
    const enText = Object.values(en).join('\n');
    const thText = Object.values(th).join('\n');
    expect(enText).toMatch(/\bKats?\b/);
    expect(thText).toMatch(/\bKats?\b/);
    expect(thText).toContain('ผู้ช่วย AI');
    expect(thText).not.toMatch(/พนักงาน\s*AI/);
    expect(enText).not.toMatch(/\bAI (employee|worker|staff)/i);
  });
});

describe('t', () => {
  test('translates_and_fills_variables', () => {
    setLocale('th');
    expect(t('reason.lease_lost')).toBe(th['reason.lease_lost']);
    setLocale('en');
    expect(t('home.today', { date: '23 October 2026' })).toBe('Today is 23 October 2026.');
  });

  test('unknown_key_returns_the_key', () => {
    expect(t('no.such.key')).toBe('no.such.key');
  });
});

describe('locale', () => {
  test('locale_is_persisted_in_local_storage', () => {
    setLocale('th');
    expect(localStorage.getItem('kw-locale')).toBe('th');
    expect(document.documentElement.lang).toBe('th');
    expect(initialLocale()).toBe('th');
    expect(getLocale()).toBe('th');
  });

  test('unreadable_storage_falls_back_to_the_browser_language', () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new DOMException('denied', 'SecurityError');
    });
    vi.spyOn(navigator, 'language', 'get').mockReturnValue('th-TH');
    expect(initialLocale()).toBe('th');
  });

  test('unwritable_storage_still_switches_locale', () => {
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new DOMException('full', 'QuotaExceededError');
    });
    setLocale('th');
    expect(getLocale()).toBe('th');
  });

  test('stored_garbage_is_ignored', () => {
    localStorage.setItem('kw-locale', 'xx');
    vi.spyOn(navigator, 'language', 'get').mockReturnValue('en-US');
    expect(initialLocale()).toBe('en');
  });
});

describe('formatDate', () => {
  const date = new Date('2026-10-23T05:00:00Z');

  test('format_date_buddhist_era_in_thai', () => {
    setLocale('th');
    const buddhist = formatDate(date, { calendar: 'buddhist', timeZone: 'Asia/Bangkok' });
    expect(buddhist).toContain('2569');
    expect(buddhist).toContain('ตุลาคม');
    expect(buddhist).not.toContain('2026');
  });

  test('ui_chrome_dates_are_gregorian_in_both_locales', () => {
    for (const locale of ['en', 'th'] as const) {
      setLocale(locale);
      const text = formatDate(date, { timeZone: 'Asia/Bangkok' });
      expect(text).toContain('2026');
      expect(text).not.toContain('2569');
    }
    setLocale('th');
    expect(formatDate(date, { timeZone: 'Asia/Bangkok' })).toContain('ตุลาคม');
  });
});
