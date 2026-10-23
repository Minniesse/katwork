import { act, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { setLocale } from '../i18n';
import en from '../i18n/en.json';
import th from '../i18n/th.json';
import { mockApi } from '../testing/api-mock';
import { FakeEventSource, installFakeEventSource } from '../testing/fake-event-source';
import { kat, TEAM, THAI_TEAM, WORKSPACE } from '../testing/fixtures';
import { TeamPage } from './Team';

beforeEach(() => {
  installFakeEventSource();
});

afterEach(() => {
  vi.unstubAllGlobals();
  act(() => setLocale('en'));
});

const TEAM_ROUTE = `GET /api/workspaces/${WORKSPACE}/teams/${TEAM}`;

async function cards() {
  const list = await screen.findByRole('list', {
    name: en['team.members'].replace('{team}', 'ทีมหลัก'),
  });
  return within(list).getAllByRole('listitem');
}

test('team_page_lists_lead_first_with_thai_names', async () => {
  mockApi({ [TEAM_ROUTE]: [200, THAI_TEAM] });
  render(<TeamPage workspaceId={WORKSPACE} teamId={TEAM} />);
  const items = await cards();
  expect(items.map((item) => within(item).getByRole('heading').textContent)).toEqual([
    'หัวหน้าทีม',
    'นักวิจัย',
    'นักเขียน',
    'นักวิเคราะห์',
    'เลขา',
    'ผู้ตรวจ',
  ]);
  expect(within(items[0]!).getByText(en['team.lead'])).toBeVisible();
  expect(items.slice(1).every((item) => within(item).queryByText(en['team.lead']) === null)).toBe(
    true,
  );
  expect(within(items[2]!).getByText(en['team.role.writer'])).toBeVisible();

  act(() => setLocale('th'));
  const thaiItems = await screen.findAllByRole('listitem');
  // The Thai lead is named หัวหน้าทีม, the same words as the tag.
  expect(within(thaiItems[0]!).getByText(th['team.lead'], { selector: '.tag' })).toBeVisible();
  // นักเขียน is both the Writer's Thai name and its role, so the role line is not repeated.
  expect(within(thaiItems[2]!).getAllByText(th['team.role.writer'])).toHaveLength(1);
  expect(within(thaiItems[3]!).getByText(th['team.status.paused'])).toBeVisible();
});

test('team_page_has_no_live_state_badge', async () => {
  mockApi({ [TEAM_ROUTE]: [200, THAI_TEAM] });
  render(<TeamPage workspaceId={WORKSPACE} teamId={TEAM} />);
  const items = await cards();
  // A run may be streaming in a thread of this team: the roster still shows only REST status.
  expect(FakeEventSource.instances).toHaveLength(0);
  expect(items.map((item) => within(item).getByTestId('kat-status').textContent)).toEqual([
    en['team.status.active'],
    en['team.status.active'],
    en['team.status.active'],
    en['team.status.paused'],
    en['team.status.active'],
    en['team.status.active'],
  ]);
  expect(screen.queryByText(/replying|Writing/)).toBeNull();
});

test('accent_palette_maps_to_tokens_and_unknown_falls_back', async () => {
  mockApi({
    [TEAM_ROUTE]: [
      200,
      {
        team: THAI_TEAM.team,
        members: [
          kat('k1', 'Green', 'lead', { appearance: { avatar: 'pen', accent: 'green' } }),
          kat('k2', 'Hex', 'writer', { appearance: { avatar: 'pen', accent: '#1A7F5A' } }),
          kat('k3', 'Injected', 'writer', { appearance: { avatar: 'pen', accent: 'red; x' } }),
        ],
      },
    ],
  });
  const { container } = render(<TeamPage workspaceId={WORKSPACE} teamId={TEAM} />);
  const items = await cards();
  const ring = (item: HTMLElement) =>
    item.querySelector<HTMLElement>('.kat-avatar')!.style.getPropertyValue('--ring');
  expect(items.map(ring)).toEqual(['var(--accent-green)', 'var(--accent)', 'var(--accent)']);
  // The accent is a decorative ring only: nothing carrying text is coloured by it.
  for (const styled of container.querySelectorAll<HTMLElement>('[style]')) {
    expect(styled).toHaveAttribute('aria-hidden', 'true');
    expect(styled.textContent).toBe('');
    expect(styled.style.color).toBe('');
    expect(styled.style.background).toBe('');
  }
});

test('unknown_avatar_shows_the_neutral_kat', async () => {
  mockApi({
    [TEAM_ROUTE]: [
      200,
      {
        team: THAI_TEAM.team,
        members: [
          kat('k1', 'Lead', 'lead', { appearance: { avatar: 'compass', accent: 'blue' } }),
          kat(
            'k2',
            'ที่ปรึกษาภาษี',
            { custom: 'ที่ปรึกษาภาษี' },
            { appearance: { avatar: 'dragon', accent: 'blue' } },
          ),
        ],
      },
    ],
  });
  render(<TeamPage workspaceId={WORKSPACE} teamId={TEAM} />);
  const items = await cards();
  const avatar = (item: HTMLElement) =>
    item.querySelector('.kat-avatar')!.getAttribute('data-avatar');
  expect(items.map(avatar)).toEqual(['compass', 'kat']);
  // A custom role shows its own name, here the same as the Kat's.
  expect(within(items[1]!).getAllByText('ที่ปรึกษาภาษี')).toHaveLength(1);
});

test('team_page_explains_a_missing_team', async () => {
  mockApi({});
  render(<TeamPage workspaceId={WORKSPACE} teamId={null} />);
  expect(await screen.findByText(en['team.none'])).toBeVisible();
});
