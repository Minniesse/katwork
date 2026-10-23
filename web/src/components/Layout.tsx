import { useEffect, useRef, type ReactNode } from 'react';
import { t, useLocale } from '../i18n';
import { navigationCount } from '../router';
import { Preferences } from './Preferences';

/** The team as six dots: the Lead in yellow and five specialists. Decorative. */
export function DotCluster({ size = 'large' }: { size?: 'large' | 'small' }) {
  return (
    <div className={`dot-cluster dot-cluster-${size}`} aria-hidden="true">
      {[1, 2, 3, 4, 5, 6].map((n) => (
        <span key={n} className={`dot dot-${n}`} />
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
      {t('app.name')}
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
        <DotCluster />
        <div className="auth-panel-copy">
          <p className="auth-panel-title">{t('auth.panel.title')}</p>
          <p>{t('auth.panel.dots')}</p>
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

/** Signed-in pages: a header with the brand, preferences and sign-out. */
export function AppLayout({ children, onSignOut }: { children: ReactNode; onSignOut: () => void }) {
  useLocale();
  return (
    <div className="shell">
      <SkipLink />
      <header className="shell-header">
        <Brand />
        <nav className="shell-nav" aria-label={t('nav.main')}>
          <Preferences />
          <button type="button" className="button button-quiet" onClick={onSignOut}>
            {t('nav.sign_out')}
          </button>
        </nav>
      </header>
      <main id="main" className="shell-main">
        {children}
      </main>
    </div>
  );
}
