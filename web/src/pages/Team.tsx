import { useEffect, useState, type CSSProperties } from 'react';
import { api, errorMessage, type Kat, type KatRole, type TeamView } from '../api';
import { PageTitle } from '../components/Layout';
import { hasMessage, t, useLocale } from '../i18n';
import { Link } from '../router';

// Task 5's palette and avatars. Anything else falls back, so a stored value is never CSS.
const ACCENTS = new Set(['blue', 'yellow', 'red', 'green', 'purple']);
const AVATARS = new Set(['compass', 'magnifier', 'pen', 'chart', 'calendar', 'check', 'kat']);

export type TeamLoad =
  | { state: 'loading' }
  | { state: 'none' }
  | { state: 'ready'; view: TeamView }
  | { state: 'failed'; error: unknown };

/** The team from `GET …/teams/{team_id}`. REST only: there is no workspace event stream. */
export function useTeam(workspaceId: string, teamId: string | null): TeamLoad {
  const [load, setLoad] = useState<TeamLoad>({ state: 'loading' });
  useEffect(() => {
    if (!teamId) {
      setLoad({ state: 'none' });
      return;
    }
    let live = true;
    setLoad({ state: 'loading' });
    api.team(workspaceId, teamId).then(
      (view) => live && setLoad({ state: 'ready', view }),
      (error: unknown) => live && setLoad({ state: 'failed', error }),
    );
    return () => {
      live = false;
    };
  }, [workspaceId, teamId]);
  return load;
}

export function roleLabel(role: KatRole): string {
  if (typeof role !== 'string') return role.custom;
  const key = `team.role.${role}`;
  return hasMessage(key) ? t(key) : role;
}

/** A decorative avatar: Task 5's avatar key, ringed in the Kat's palette accent. */
export function KatAvatar({ appearance }: { appearance: Kat['appearance'] }) {
  const avatar = AVATARS.has(appearance.avatar) ? appearance.avatar : 'kat';
  const ring = ACCENTS.has(appearance.accent)
    ? `var(--accent-${appearance.accent})`
    : 'var(--accent)';
  return (
    <span
      className="kat-avatar"
      data-avatar={avatar}
      aria-hidden="true"
      style={{ '--ring': ring } as CSSProperties}
    >
      <svg viewBox="0 0 24 24" focusable="false">
        <path className="kat-avatar-ears" d="M5 9 6 3l4 3.5M19 9l-1-6-4 3.5" />
        <path d={GLYPHS[avatar]} />
      </svg>
    </span>
  );
}

const GLYPHS: Record<string, string> = {
  compass: 'M12 7.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11Zm2 3.5-1.2 2.8L10 15l1.2-2.8Z',
  magnifier: 'M11 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8Zm3 7 3.5 3.5',
  pen: 'm7.5 18 1-3.5 6-6 2.5 2.5-6 6Zm6-8.5 2.5 2.5',
  chart: 'M7.5 18v-3m3 3v-6m3 6v-4.5m3 4.5V10',
  calendar: 'M7 10h10v8H7Zm0 2.5h10M9.5 8.5v3m5-3v3',
  check: 'm8 13.5 2.8 2.8L16.5 10',
  kat: 'M9.5 13h.01M14.5 13h.01M10.5 16c.9.7 2.1.7 3 0',
};

/** The roster, in the order the server returns it: the lead first, then by position. */
export function TeamRoster({ view, compact = false }: { view: TeamView; compact?: boolean }) {
  useLocale();
  return (
    <ul
      className={`roster${compact ? ' roster-compact' : ''}`}
      aria-label={t('team.members', { team: view.team.name })}
    >
      {view.members.map((member, index) => {
        const role = roleLabel(member.role);
        // The tag already says lead, and the Thai roster names Kats after their roles.
        const showRole = !(index === 0 && member.role === 'lead') && role !== member.name;
        return (
          <li key={member.id} className="kat-card">
            <KatAvatar appearance={member.appearance} />
            <div className="kat-card-text">
              <h3 className="kat-name">{member.name}</h3>
              {showRole && <p className="kat-role">{role}</p>}
            </div>
            <div className="kat-card-tags">
              {index === 0 && <span className="tag">{t('team.lead')}</span>}
              <span className={`badge badge-${member.status}`} data-testid="kat-status">
                {t(`team.status.${member.status}`)}
              </span>
            </div>
          </li>
        );
      })}
    </ul>
  );
}

export function TeamPage({ workspaceId, teamId }: { workspaceId: string; teamId: string | null }) {
  useLocale();
  const load = useTeam(workspaceId, teamId);
  return (
    <div className="page">
      <div className="page-intro">
        <PageTitle>{t('team.title')}</PageTitle>
        <p className="lede">{t('team.lede')}</p>
      </div>
      <TeamBody load={load} />
      {load.state === 'ready' && (
        <p>
          <Link to="/chat" className="link">
            {t('team.chat')}
          </Link>
        </p>
      )}
    </div>
  );
}

export function TeamBody({ load, compact = false }: { load: TeamLoad; compact?: boolean }) {
  useLocale();
  switch (load.state) {
    case 'loading':
      return (
        <p className="hint" aria-busy="true">
          {t('team.loading')}
        </p>
      );
    case 'none':
      return <p>{t('team.none')}</p>;
    case 'failed':
      return (
        <div className="alert" role="alert">
          <p>{errorMessage(load.error)}</p>
        </div>
      );
    case 'ready':
      return (
        <section className="team" aria-labelledby="team-name">
          <h2 id="team-name">{load.view.team.name}</h2>
          <TeamRoster view={load.view} compact={compact} />
        </section>
      );
  }
}
