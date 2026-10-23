import { KatCluster, PageTitle } from '../components/Layout';
import { formatDate, t, useLocale } from '../i18n';
import { Link } from '../router';
import { Conversations } from './Chat';
import { TeamRoster, useTeam } from './Team';

/** The signed-in landing page: the Space's team and its latest conversations. */
export function Home({
  workspaceId,
  spaceId,
  teamId,
}: {
  workspaceId: string;
  spaceId: string;
  teamId: string | null;
}) {
  useLocale();
  const team = useTeam(workspaceId, teamId);
  return (
    <div className="page">
      <div className="home-intro">
        <PageTitle>{t('home.title')}</PageTitle>
        <p className="home-date">{t('home.today', { date: formatDate(new Date()) })}</p>
        <p className="lede">{t('home.lede')}</p>
      </div>
      <div className="home-grid">
        <section className="panel panel-team" aria-labelledby="home-team-title">
          <h2 id="home-team-title">{t('home.team.title')}</h2>
          {team.state === 'ready' ? (
            <TeamRoster view={team.view} compact />
          ) : (
            <>
              <KatCluster size="small" />
              {team.state === 'none' && <p>{t('team.none')}</p>}
            </>
          )}
          <p>
            <Link to="/team" className="link">
              {t('home.team.open')}
            </Link>
          </p>
        </section>
        <section className="panel panel-chat" aria-labelledby="home-chat-title">
          <h2 id="home-chat-title">{t('home.chat.title')}</h2>
          <Conversations
            workspaceId={workspaceId}
            spaceId={spaceId}
            teamId={teamId}
            limit={5}
            compact
          />
          <p>
            <Link to="/chat" className="link">
              {t('home.chat.open')}
            </Link>
          </p>
        </section>
      </div>
    </div>
  );
}
