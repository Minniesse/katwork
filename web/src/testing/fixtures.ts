// Test-only API payloads: a workspace with one Space and its Thai default team.
import type { Kat, TeamView, Thread, WorkspaceSummary } from '../api';

export const WORKSPACE = 'ws-1';
export const SPACE = 'space-1';
export const TEAM = 'team-1';
export const THREAD = 'thread-1';
export const ME = 'user-1';

export function kat(
  id: string,
  name: string,
  role: Kat['role'],
  overrides: Partial<Kat> = {},
): Kat {
  return {
    id,
    name,
    role,
    status: 'active',
    appearance: { avatar: 'kat', accent: 'blue' },
    ...overrides,
  };
}

/** The Thai default roster, the lead first, as `GET …/teams/{team_id}` returns it. */
export const THAI_TEAM: TeamView = {
  team: { id: TEAM, space_id: SPACE, name: 'ทีมหลัก' },
  members: [
    kat('kat-lead', 'หัวหน้าทีม', 'lead', { appearance: { avatar: 'compass', accent: 'yellow' } }),
    kat('kat-researcher', 'นักวิจัย', 'researcher', {
      appearance: { avatar: 'magnifier', accent: 'blue' },
    }),
    kat('kat-writer', 'นักเขียน', 'writer', { appearance: { avatar: 'pen', accent: 'green' } }),
    kat('kat-analyst', 'นักวิเคราะห์', 'analyst', {
      status: 'paused',
      appearance: { avatar: 'chart', accent: 'purple' },
    }),
    kat('kat-scheduler', 'เลขา', 'scheduler', {
      appearance: { avatar: 'calendar', accent: 'red' },
    }),
    kat('kat-reviewer', 'ผู้ตรวจ', 'reviewer', { appearance: { avatar: 'check', accent: 'blue' } }),
  ],
};

export const THREAD_ROW: Thread = {
  id: THREAD,
  space_id: SPACE,
  team_id: TEAM,
  title: 'แผนงานไตรมาส 4',
};

export const WORKSPACES: WorkspaceSummary[] = [
  { id: WORKSPACE, name: 'เดโม', role: 'owner', paused: false },
];
