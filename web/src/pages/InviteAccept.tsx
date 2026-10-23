import { useId, useState, type FormEvent } from 'react';
import { ApiError, api, errorMessage } from '../api';
import { Announce } from '../components/Announce';
import { AuthLayout, PageTitle } from '../components/Layout';
import { t, useLocale } from '../i18n';
import { Link, useLinkToken } from '../router';

// Matches katwork::auth::password: at least 8 characters, at most 1024 bytes.
const MIN_CHARS = 8;
const MAX_BYTES = 1024;

type Problem = { kind: 'api'; error: unknown } | { kind: 'mismatch' };

/** Where an invite link lands: the invited person chooses their first password. */
export function InviteAccept({ onAccepted }: { onAccepted?: () => void }) {
  useLocale();
  // Held in memory only: see useLinkToken.
  const token = useLinkToken('/invite');
  const id = useId();
  const [password, setPassword] = useState('');
  const [again, setAgain] = useState('');
  const [problem, setProblem] = useState<Problem | null>(null);
  const [done, setDone] = useState(false);
  const [busy, setBusy] = useState(false);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (busy || !token) return;
    if ([...password].length < MIN_CHARS || new TextEncoder().encode(password).length > MAX_BYTES) {
      setProblem({ kind: 'api', error: new ApiError(0, 'password_invalid') });
      return;
    }
    if (password !== again) {
      setProblem({ kind: 'mismatch' });
      return;
    }
    setBusy(true);
    setProblem(null);
    try {
      await api.acceptInvite(token, password);
      setDone(true);
      onAccepted?.();
    } catch (failure) {
      setProblem({ kind: 'api', error: failure });
    } finally {
      setBusy(false);
    }
  };

  if (done) {
    return (
      <AuthLayout>
        <PageTitle>{t('invite.title')}</PageTitle>
        <Announce>
          <p>{t('invite.done')}</p>
          <p>
            <Link to="/login" className="link">
              {t('reset.back_to_login')}
            </Link>
          </p>
        </Announce>
      </AuthLayout>
    );
  }

  const errorId = `${id}-error`;
  // A link without a token can never work: say so instead of showing a form. Neither message
  // links to password reset, which is a dead end on an install without SMTP; both point at
  // the person who manages KatWork instead.
  const alert = (problem || !token) && (
    <div id={errorId} className="alert" role="alert">
      <p>
        {!token
          ? t('invite.no_token')
          : problem?.kind === 'mismatch'
            ? t('invite.mismatch')
            : errorMessage(problem?.error)}
      </p>
    </div>
  );
  return (
    <AuthLayout>
      <PageTitle>{t('invite.title')}</PageTitle>
      <p className="lede">{t('invite.lede')}</p>
      {!token ? (
        alert
      ) : (
        <form className="form" onSubmit={submit} noValidate>
          {alert}
          <div className="field">
            <label htmlFor={`${id}-password`}>{t('invite.password')}</label>
            <input
              id={`${id}-password`}
              type="password"
              autoComplete="new-password"
              required
              minLength={MIN_CHARS}
              value={password}
              aria-invalid={problem !== null || undefined}
              aria-describedby={[`${id}-hint`, problem ? errorId : ''].join(' ').trim()}
              onChange={(event) => setPassword(event.target.value)}
            />
            <p id={`${id}-hint`} className="hint">
              {t('invite.hint')}
            </p>
          </div>
          <div className="field">
            <label htmlFor={`${id}-again`}>{t('invite.again')}</label>
            <input
              id={`${id}-again`}
              type="password"
              autoComplete="new-password"
              required
              value={again}
              aria-invalid={problem?.kind === 'mismatch' || undefined}
              onChange={(event) => setAgain(event.target.value)}
            />
          </div>
          <div className="form-actions">
            <button type="submit" className="button button-primary" aria-disabled={busy}>
              {busy ? t('invite.saving') : t('invite.submit')}
            </button>
          </div>
        </form>
      )}
    </AuthLayout>
  );
}
