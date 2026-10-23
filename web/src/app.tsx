import { useEffect, useState, type ReactNode } from 'react';
import { api, ApiError, errorMessage, type AuthUser } from './api';
import { AppLayout, AuthLayout, PageTitle } from './components/Layout';
import { t, useLocale } from './i18n';
import { Chat } from './pages/Chat';
import { Home } from './pages/Home';
import { InviteAccept } from './pages/InviteAccept';
import { Login } from './pages/Login';
import { Members } from './pages/Members';
import { ResetConfirm } from './pages/ResetConfirm';
import { ResetRequest } from './pages/ResetRequest';
import { TeamPage } from './pages/Team';
import { Link, navigate, useLinkToken, useLocation } from './router';
import { useWorkspace, type WorkspaceContext } from './workspace';

type Session = { state: 'checking' } | { state: 'signed_out' } | { state: 'signed_in'; user: AuthUser };

type Page =
  | { page: 'home' }
  | { page: 'chat'; threadId: string | null }
  | { page: 'team'; teamId: string | null }
  | { page: 'members' };

// `/chat/:threadId?`, `/team/:teamId?`, `/members` and `/`.
function route(path: string): Page | null {
  if (path === '/') return { page: 'home' };
  if (path === '/members') return { page: 'members' };
  const match = /^\/(chat|team)(?:\/([^/]+))?\/?$/.exec(path);
  if (!match) return null;
  const id = match[2] ? decodeURIComponent(match[2]) : null;
  return match[1] === 'chat' ? { page: 'chat', threadId: id } : { page: 'team', teamId: id };
}

export function App() {
  useLocale();
  const location = useLocation();
  const [session, setSession] = useState<Session>({ state: 'checking' });
  // Bumped to ask the server again who is signed in.
  const [sessionCheck, setSessionCheck] = useState(0);
  const [signOutProblem, setSignOutProblem] = useState<{ error: unknown } | null>(null);
  // The token from an emailed or printed `/reset?token=…` link, held in memory only: see useLinkToken.
  const token = useLinkToken('/reset');
  const linkPage = location.pathname === '/reset' || location.pathname === '/invite';
  const [workspace, chooseWorkspace, chooseSpace, reloadWorkspace] = useWorkspace(
    session.state === 'signed_in' && !linkPage,
  );

  useEffect(() => {
    let live = true;
    api.me().then(
      (user) => live && setSession({ state: 'signed_in', user }),
      () => live && setSession({ state: 'signed_out' }),
    );
    return () => {
      live = false;
    };
  }, [sessionCheck]);

  const path = location.pathname;
  const signedIn = session.state === 'signed_in';
  const signedOut = session.state === 'signed_out';

  useEffect(() => {
    if (path === '/reset' || path === '/invite') return;
    if (signedOut && path !== '/login') navigate('/login', { replace: true });
    if (signedIn && path === '/login') navigate('/', { replace: true });
  }, [path, signedIn, signedOut]);

  const signOut = async () => {
    setSignOutProblem(null);
    try {
      await api.logout();
    } catch (error) {
      // The server clears the cookie only after it revokes the session, so the person is
      // still signed in: say so, and leave the button there to try again.
      setSignOutProblem({ error });
      return;
    }
    setSession({ state: 'signed_out' });
    navigate('/login');
  };

  // A reset revokes every session of the account it was for, which may be this one.
  const recheckSession = () => {
    setSession({ state: 'checking' });
    setSessionCheck((n) => n + 1);
  };

  if (path === '/reset') {
    return token ? (
      <ResetConfirm key={token} token={token} onReset={recheckSession} />
    ) : (
      <ResetRequest />
    );
  }
  // An invite link works signed in or out; accepting it ends every session of that account.
  if (path === '/invite') return <InviteAccept onAccepted={recheckSession} />;
  if (session.state === 'checking') {
    return <div className="loading" aria-busy="true" />;
  }
  if (path === '/login' && signedOut) {
    return (
      <Login
        onSignedIn={(user) => {
          setSession({ state: 'signed_in', user });
          navigate('/', { replace: true });
        }}
      />
    );
  }
  if (!signedIn) return null;
  const page = route(path);
  if (!page) return <NotFound />;

  // The server ended this session (a stream or request answered 401).
  const sessionEnded = () => {
    setSession({ state: 'signed_out' });
    navigate('/login', { replace: true });
  };
  const nav =
    workspace.state === 'ready'
      ? {
          path,
          workspaces: workspace.workspaces,
          workspace: workspace.workspace,
          spaces: workspace.spaces,
          space: workspace.space,
          onWorkspace: chooseWorkspace,
          onSpace: chooseSpace,
        }
      : null;
  return (
    <AppLayout onSignOut={signOut} signOutProblem={signOutProblem} nav={nav}>
      <SignedInPage
        page={page}
        workspace={workspace}
        me={session.user.user_id}
        onSessionEnded={sessionEnded}
        onRolesChanged={reloadWorkspace}
      />
    </AppLayout>
  );
}

function SignedInPage({
  page,
  workspace,
  me,
  onSessionEnded,
  onRolesChanged,
}: {
  page: Page;
  workspace: WorkspaceContext;
  me: string;
  onSessionEnded: () => void;
  onRolesChanged: () => void;
}): ReactNode {
  useLocale();
  const failed = workspace.state === 'failed';
  const unauthenticated = failed && workspace.error instanceof ApiError && workspace.error.status === 401;
  useEffect(() => {
    if (unauthenticated) onSessionEnded();
  }, [unauthenticated, onSessionEnded]);

  switch (workspace.state) {
    case 'loading':
      return (
        <p className="hint" aria-busy="true">
          {t('workspace.loading')}
        </p>
      );
    case 'failed':
      return (
        <div className="alert" role="alert">
          <p>{errorMessage(workspace.error)}</p>
        </div>
      );
    case 'none':
      return <p>{t('workspace.none')}</p>;
    case 'ready':
      break;
  }
  const { workspace: current, space, teamId } = workspace;
  if (page.page === 'members') {
    return (
      <Members
        key={current.id}
        workspaceId={current.id}
        workspaceName={current.name}
        me={me}
        myRole={current.role}
        onOwnRoleChanged={onRolesChanged}
      />
    );
  }
  if (page.page === 'team') {
    return <TeamPage workspaceId={current.id} teamId={page.teamId ?? teamId} />;
  }
  if (!space) return <p>{t('workspace.no_space')}</p>;
  if (page.page === 'chat') {
    return (
      <Chat
        workspaceId={current.id}
        spaceId={space.id}
        teamId={teamId}
        threadId={page.threadId}
        me={me}
        onSignedOut={onSessionEnded}
      />
    );
  }
  return <Home workspaceId={current.id} spaceId={space.id} teamId={teamId} />;
}

function NotFound() {
  return (
    <AuthLayout>
      <PageTitle>{t('reason.not_found')}</PageTitle>
      <p>
        <Link to="/" className="link">
          {t('home.title')}
        </Link>
      </p>
    </AuthLayout>
  );
}
