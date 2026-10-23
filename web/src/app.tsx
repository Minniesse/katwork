import { useEffect, useState } from 'react';
import { api, type AuthUser } from './api';
import { AuthLayout, PageTitle } from './components/Layout';
import { t, useLocale } from './i18n';
import { Home } from './pages/Home';
import { Login } from './pages/Login';
import { ResetConfirm } from './pages/ResetConfirm';
import { ResetRequest } from './pages/ResetRequest';
import { Link, navigate, useLocation } from './router';

type Session = { state: 'checking' } | { state: 'signed_out' } | { state: 'signed_in'; user: AuthUser };

/**
 * The token from an emailed `/reset?token=…` link. Once read it moves from the URL into the
 * history entry's state, so it stays out of the address bar and any later Referer, survives
 * a reload, and is gone as soon as the person navigates anywhere else.
 */
function resetToken(location: URL): string | null {
  if (location.pathname !== '/reset') return null;
  const fromLink = location.searchParams.get('token');
  if (fromLink) return fromLink;
  const state: unknown = window.history.state;
  const saved = (state as { resetToken?: unknown } | null)?.resetToken;
  return typeof saved === 'string' && saved ? saved : null;
}

export function App() {
  useLocale();
  const location = useLocation();
  const [session, setSession] = useState<Session>({ state: 'checking' });
  // Bumped to ask the server again who is signed in.
  const [sessionCheck, setSessionCheck] = useState(0);
  const [signOutProblem, setSignOutProblem] = useState<{ error: unknown } | null>(null);
  const token = resetToken(location);

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

  const tokenInUrl = location.pathname === '/reset' && location.searchParams.has('token');
  useEffect(() => {
    if (tokenInUrl) navigate('/reset', { replace: true, state: { resetToken: token } });
  }, [tokenInUrl, token]);

  const path = location.pathname;
  const signedIn = session.state === 'signed_in';
  const signedOut = session.state === 'signed_out';

  useEffect(() => {
    if (path === '/reset') return;
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
  if (path === '/') return <Home onSignOut={signOut} signOutProblem={signOutProblem} />;
  // TODO(13b): routes for the Team and Chat pages.
  return <NotFound />;
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
