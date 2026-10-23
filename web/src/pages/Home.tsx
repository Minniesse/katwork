import { AppLayout, DotCluster, PageTitle } from '../components/Layout';
import { formatDate, t, useLocale } from '../i18n';

/** The signed-in landing page. Team and Chat arrive in Task 13b. */
export function Home({
  onSignOut,
  signOutProblem,
}: {
  onSignOut: () => void;
  signOutProblem: { error: unknown } | null;
}) {
  useLocale();
  return (
    <AppLayout onSignOut={onSignOut} signOutProblem={signOutProblem}>
      <div className="home-intro">
        <PageTitle>{t('home.title')}</PageTitle>
        <p className="home-date">{t('home.today', { date: formatDate(new Date()) })}</p>
        <p className="lede">{t('home.lede')}</p>
      </div>
      <div className="home-grid">
        {/* TODO(13b): replace with the Team page (Tasks 4 and 5): the roster from the teams
            route, Lead first, with Thai names (`team_page_lists_lead_first_with_thai_names`). */}
        <section className="panel panel-team" aria-labelledby="home-team-title">
          <h2 id="home-team-title">{t('home.team.title')}</h2>
          <DotCluster size="small" />
          <p>{t('home.team.empty')}</p>
          <p className="tag">{t('home.coming_soon')}</p>
        </section>
        {/* TODO(13b): replace with the Chat page (Tasks 6, 9, 10 and 12): <Transcript> fed by
            subscribe() and foldEvent() from a messages snapshot, <Composer> posting with a
            client_msg_id, and a Stop button that calls the cancel route while a job runs. */}
        <section className="panel panel-chat" aria-labelledby="home-chat-title">
          <h2 id="home-chat-title">{t('home.chat.title')}</h2>
          <p>{t('home.chat.empty')}</p>
          <p className="tag">{t('home.coming_soon')}</p>
        </section>
      </div>
    </AppLayout>
  );
}
