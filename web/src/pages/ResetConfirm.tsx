import { useId, useState, type FormEvent } from 'react';
import { ApiError, api, errorMessage } from '../api';
import { Announce } from '../components/Announce';
import { AuthLayout, PageTitle } from '../components/Layout';
import { t, useLocale } from '../i18n';
import { Link } from '../router';

// Matches katwork::auth::password: at least 8 characters, at most 1024 bytes.
const MIN_CHARS = 8;
const MAX_BYTES = 1024;

type Problem = { kind: 'api'; error: unknown } | { kind: 'mismatch' };

/** `onReset` runs after the server accepts the new password and revokes the account's sessions. */
export function ResetConfirm({ token, onReset }: { token: string; onReset: () => void }) {
  useLocale();
  const id = useId();
  const [password, setPassword] = useState('');
  const [again, setAgain] = useState('');
  const [problem, setProblem] = useState<Problem | null>(null);
  const [done, setDone] = useState(false);
  const [busy, setBusy] = useState(false);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
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
      await api.confirmReset(token, password);
      setDone(true);
      onReset();
    } catch (failure) {
      setProblem({ kind: 'api', error: failure });
    } finally {
      setBusy(false);
    }
  };

  if (done) {
    return (
      <AuthLayout>
        <PageTitle>{t('reset.confirm.title')}</PageTitle>
        <Announce>
          <p>{t('reset.confirm.done')}</p>
          <p>
            <Link to="/login" className="link">
              {t('reset.back_to_login')}
            </Link>
          </p>
        </Announce>
      </AuthLayout>
    );
  }

  const tokenRejected =
    problem?.kind === 'api' &&
    problem.error instanceof ApiError &&
    problem.error.code === 'reset_token_invalid';
  const errorId = `${id}-error`;
  return (
    <AuthLayout>
      <PageTitle>{t('reset.confirm.title')}</PageTitle>
      <p className="lede">{t('reset.confirm.lede')}</p>
      <form className="form" onSubmit={submit} noValidate>
        {problem && (
          <div id={errorId} className="alert" role="alert">
            <p>
              {problem.kind === 'mismatch'
                ? t('reset.confirm.mismatch')
                : errorMessage(problem.error)}
            </p>
            {tokenRejected && (
              <p>
                <Link to="/reset" className="link">
                  {t('reset.request_again')}
                </Link>
              </p>
            )}
          </div>
        )}
        <div className="field">
          <label htmlFor={`${id}-password`}>{t('reset.confirm.password')}</label>
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
            {t('reset.confirm.hint')}
          </p>
        </div>
        <div className="field">
          <label htmlFor={`${id}-again`}>{t('reset.confirm.again')}</label>
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
            {busy ? t('reset.confirm.saving') : t('reset.confirm.submit')}
          </button>
        </div>
      </form>
    </AuthLayout>
  );
}
