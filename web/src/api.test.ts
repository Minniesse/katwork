import { afterEach, describe, expect, test, vi } from 'vitest';
import { ApiError, api, apiFetch, errorMessage } from './api';
import { setLocale } from './i18n';
import en from './i18n/en.json';
import th from './i18n/th.json';

function mockFetch(status: number, body?: unknown) {
  const fetchMock = vi.fn(
    async (_input: RequestInfo | URL, _init?: RequestInit) =>
      new Response(body === undefined ? null : JSON.stringify(body), {
        status,
        headers: body === undefined ? {} : { 'content-type': 'application/json' },
      }),
  );
  vi.stubGlobal('fetch', fetchMock);
  return fetchMock;
}

afterEach(() => {
  vi.unstubAllGlobals();
  setLocale('en');
});

describe('apiFetch', () => {
  test('posts_json_same_origin_with_credentials', async () => {
    const fetchMock = mockFetch(200, { user_id: 'u1' });
    await api.login('person@example.test', 'correct horse');
    const [url, init] = fetchMock.mock.calls[0]!;
    expect(url).toBe('/api/auth/login');
    expect(init?.method).toBe('POST');
    expect(init?.credentials).toBe('same-origin');
    expect(init?.mode).toBe('same-origin');
    expect(new Headers(init?.headers).get('content-type')).toBe('application/json');
    expect(new Headers(init?.headers).get('accept')).toBe('application/json');
    expect(JSON.parse(String(init?.body))).toEqual({
      email: 'person@example.test',
      password: 'correct horse',
    });
  });

  test('get_sends_no_body', async () => {
    const fetchMock = mockFetch(200, { user_id: 'u1' });
    await expect(api.me()).resolves.toEqual({ user_id: 'u1' });
    const [, init] = fetchMock.mock.calls[0]!;
    expect(init?.method).toBe('GET');
    expect(init?.body).toBeUndefined();
  });

  test('accepted_with_empty_json_resolves', async () => {
    mockFetch(202, {});
    await expect(api.requestReset('someone@example.test')).resolves.toBeUndefined();
  });

  test('refuses_anything_but_a_same_origin_api_path', async () => {
    const fetchMock = mockFetch(200, {});
    for (const path of ['https://evil.example/api/x', '//evil.example/api/x', '/healthz', 'api/x']) {
      await expect(apiFetch(path)).rejects.toThrow();
    }
    expect(fetchMock).not.toHaveBeenCalled();
  });

  test('error_body_code_becomes_api_error', async () => {
    mockFetch(401, { code: 'unauthenticated' });
    const error = await api.login('a@example.test', 'nope').catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ApiError);
    expect(error).toMatchObject({ status: 401, code: 'unauthenticated' });
  });

  test('error_without_code_maps_to_unknown', async () => {
    mockFetch(503);
    await expect(api.me()).rejects.toMatchObject({ status: 503, code: 'unknown' });
  });

  test('network_failure_maps_to_network_error', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => Promise.reject(new TypeError('Failed to fetch'))));
    await expect(api.me()).rejects.toMatchObject({ status: 0, code: 'network_error' });
  });
});

describe('errorMessage', () => {
  test('maps_codes_to_localized_reason_messages', () => {
    expect(errorMessage(new ApiError(429, 'rate_limited'))).toBe(en['reason.rate_limited']);
    setLocale('th');
    expect(errorMessage(new ApiError(429, 'rate_limited'))).toBe(th['reason.rate_limited']);
  });

  test('unknown_or_unsafe_codes_fall_back_to_the_generic_message', () => {
    expect(errorMessage(new ApiError(500, 'something_new'))).toBe(en['reason.unknown']);
    expect(errorMessage(new ApiError(500, '__proto__'))).toBe(en['reason.unknown']);
    expect(errorMessage(new Error('boom'))).toBe(en['reason.unknown']);
  });
});
