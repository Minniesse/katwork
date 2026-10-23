import { useEffect, useRef, type ReactNode } from 'react';
import { errorMessage, type Space, type WorkspaceSummary } from '../api';
import { t, useLocale } from '../i18n';
import { Link, navigationCount } from '../router';
import { Preferences } from './Preferences';

/** The team as six Kats: the Lead in yellow and five specialists. Decorative. */
export function KatCluster({ size = 'large' }: { size?: 'large' | 'small' }) {
  return (
    <div className={`kat-cluster kat-cluster-${size}`} aria-hidden="true">
      {[1, 2, 3, 4, 5, 6].map((n) => (
        <span key={n} className={`kat kat-${n}`} />
      ))}
    </div>
  );
}

function Brand() {
  return (
    <p className="brand">
      <span className="brand-mark" aria-hidden="true">
        <span />
        <span />
        <span />
      </span>
      <span className="brand-name">{t('app.name')}</span>
    </p>
  );
}

function SkipLink() {
  return (
    <a className="skip-link" href="#main">
      {t('nav.skip')}
    </a>
  );
}

/** Sets the tab title and moves focus to the page heading after an in-app navigation. */
export function PageTitle({ children, focus = true }: { children: string; focus?: boolean }) {
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => {
    document.title = `${children} | ${t('app.name')}`;
  }, [children]);
  useEffect(() => {
    if (focus && navigationCount() > 0) heading.current?.focus();
  }, [focus]);
  return (
    <h1 ref={heading} tabIndex={-1}>
      {children}
    </h1>
  );
}

/** Sign-in and reset pages: a blue panel with the team on the left, the form on the right. */
export function AuthLayout({ children }: { children: ReactNode }) {
  useLocale();
  return (
    <div className="auth">
      <SkipLink />
      <aside className="auth-panel">
        <Brand />
        <KatCluster />
        <div className="auth-panel-copy">
          <p className="auth-panel-title">{t('auth.panel.title')}</p>
          <p>{t('auth.panel.kats')}</p>
          <p className="auth-panel-trust">{t('auth.panel.trust')}</p>
        </div>
      </aside>
      <div className="auth-main">
        <Preferences />
        <main id="main" className="auth-form">
          {children}
        </main>
      </div>
    </div>
  );
}

export interface NavContext {
  path: string;
  workspaces: WorkspaceSummary[];
  workspace: WorkspaceSummary;
  spaces: Space[];
  space: Space | null;
  onWorkspace: (workspaceId: string) => void;
  onSpace: (spaceId: string) => void;
}

function NavLink({ to, path, children }: { to: string; path: string; children: string }) {
  const current = to === '/' ? path === '/' : path === to || path.startsWith(`${to}/`);
  return (
    <Link to={to} className="nav-link" aria-current={current ? 'page' : undefined}>
      {children}
    </Link>
  );
}

/** A workspace or Space picker, shown only when there is a choice to make. */
function Picker<T extends { id: string; name: string }>({
  label,
  items,
  current,
  onPick,
}: {
  label: string;
  items: T[];
  current: T | null;
  onPick: (id: string) => void;
}) {
  if (items.length < 2 || !current) return null;
  return (
    <label className="picker">
      <span className="picker-label">{label}</span>
      <select className="select" value={current.id} onChange={(event) => onPick(event.target.value)}>
        {items.map((item) => (
          <option key={item.id} value={item.id}>
            {item.name}
          </option>
        ))}
      </select>
    </label>
  );
}

/**
 * Signed-in pages: a header with the brand, the page links, preferences and sign-out, and an
 * alert above the page when signing out failed. The Members link shows only to the workspace's
 * owners and admins.
 */
export function AppLayout({
  children,
  onSignOut,
  signOutProblem,
  nav = null,
}: {
  children: ReactNode;
  onSignOut: () => void;
  signOutProblem: { error: unknown } | null;
  nav?: NavContext | null;
}) {
  useLocale();
  const admin = nav !== null && nav.workspace.role !== 'member';
  return (
    <div className="shell">
      <SkipLink />
      <header className="shell-header">
        <Brand />
        {nav && (
          <nav className="shell-nav" aria-label={t('nav.main')}>
            <ul className="nav-links">
              <li>
                <NavLink to="/" path={nav.path}>
                  {t('nav.home')}
                </NavLink>
              </li>
              <li>
                <NavLink to="/chat" path={nav.path}>
                  {t('nav.chat')}
                </NavLink>
              </li>
              <li>
                <NavLink to="/team" path={nav.path}>
                  {t('nav.team')}
                </NavLink>
              </li>
              {admin && (
                <li>
                  <NavLink to="/members" path={nav.path}>
                    {t('nav.members')}
                  </NavLink>
                </li>
              )}
            </ul>
            <div className="pickers">
              <Picker
                label={t('nav.workspace')}
                items={nav.workspaces}
                current={nav.workspace}
                onPick={nav.onWorkspace}
              />
              <Picker
                label={t('nav.space')}
                items={nav.spaces}
                current={nav.space}
                onPick={nav.onSpace}
              />
            </div>
          </nav>
        )}
        <div className="shell-tools">
          <Preferences />
          <button type="button" className="button button-quiet" onClick={onSignOut}>
            {t('nav.sign_out')}
          </button>
        </div>
      </header>
      <main id="main" className="shell-main">
        {signOutProblem && (
          <div className="alert" role="alert">
            <p>{t('nav.sign_out_failed')}</p>
            <p>{errorMessage(signOutProblem.error)}</p>
          </div>
        )}
        {children}
      </main>
    </div>
  );
}
