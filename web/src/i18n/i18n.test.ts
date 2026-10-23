import { afterEach, describe, expect, test, vi } from 'vitest';
import en from './en.json';
import th from './th.json';
import { formatDate, getLocale, initialLocale, setLocale, t, tPlural } from './index';

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

// API error codes the routes and guards can return (KatError::code) that a page shows.
const API_CODES = [
  'forbidden',
  'member_exists',
  'last_owner_required',
  'smtp_unconfigured',
  'message_empty',
  'message_invalid',
  'message_too_large',
  'thread_title_invalid',
  'client_msg_id_conflict',
  'busy',
  'too_many_streams',
  'unauthenticated',
  'reset_token_invalid',
  'invite_token_invalid',
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

  test('stopped_pause_notices_explain_how_to_request_work_again', () => {
    expect(en['reason.workspace_paused_stopped']).toBe('Stopped because the Kats in this workspace were paused. Send the request again after they resume.');
    expect(en['reason.kat_paused_stopped']).toBe('Stopped because this Kat was paused. Send the request again after it resumes, or ask another Kat.');
    expect(th['reason.workspace_paused_stopped']).toBe('งานหยุดแล้ว เพราะ Kats ในเวิร์กสเปซนี้ถูกหยุดไว้ชั่วคราว โปรดส่งคำขออีกครั้งเมื่อเปิดใช้แล้ว');
    expect(th['reason.kat_paused_stopped']).toBe('งานหยุดแล้ว เพราะ Kat ที่รับงานนี้ถูกหยุดไว้ชั่วคราว โปรดส่งคำขออีกครั้งเมื่อเปิดใช้แล้ว หรือขอให้ Kat อื่นช่วยแทน');
  });

  test('thai_copy_names_a_space_one_way_and_asks_politely', () => {
    // The nav calls a Space พื้นที่, so no Thai copy says "Space".
    for (const [key, value] of Object.entries(th)) expect(value, key).not.toMatch(/\bSpace\b/);
    for (const key of [
      'workspace.none',
      'workspace.no_space',
      'team.none',
      'members.invite.link_hint',
      'reason.message_empty',
      'reason.thread_title_invalid',
      'reason.too_many_streams',
    ]) {
      expect((th as Record<string, string>)[key], key).toContain('โปรด');
    }
    expect(th['reason.thread_title_invalid']).not.toContain('อักขระควบคุม');
    expect(th['workspace.no_space']).toContain('เพื่อเพิ่มพื้นที่');
    expect(th['team.none']).toContain('เพื่อตั้งทีม');
    expect(th['chat.untitled_at']).toContain('เริ่มเมื่อ {time}');
    expect(en['reason.thread_title_invalid']).not.toMatch(/control character/i);
  });

  test('lost_link_hint_says_a_reset_link_lasts_30_minutes', () => {
    // `reset-link` prints a reset link (30 minutes), not an invite link (72 hours).
    expect(en['members.invite.link_lost']).toContain('30 minutes');
    expect(en['members.invite.link_lost']).not.toContain('72');
    expect(th['members.invite.link_lost']).toContain('30 นาที');
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

describe('plural', () => {
  test('english_counts_one_and_other_and_thai_has_one_form', () => {
    expect(tPlural('chat.earlier_loaded', 1)).toBe('1 earlier message loaded');
    expect(tPlural('chat.earlier_loaded', 50)).toBe('50 earlier messages loaded');
    setLocale('th');
    expect(tPlural('chat.earlier_loaded', 1)).toBe('โหลดข้อความก่อนหน้าแล้ว 1 ข้อความ');
    expect(tPlural('chat.earlier_loaded', 50)).toBe('โหลดข้อความก่อนหน้าแล้ว 50 ข้อความ');
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
