import { useId, useState, type FormEvent } from 'react';
import { ApiError, api, errorMessage, type AuthUser } from '../api';
import { AuthLayout, PageTitle } from '../components/Layout';
import { t, useLocale } from '../i18n';
import { Link } from '../router';

export function Login({ onSignedIn }: { onSignedIn: (user: AuthUser) => void }) {
  useLocale();
  const id = useId();
  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    if (!email.trim() || !password) {
      setError(new ApiError(0, 'request_invalid'));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      onSignedIn(await api.login(email.trim(), password));
    } catch (failure) {
      setError(failure);
      setBusy(false);
    }
  };

  const invalid = error !== null;
  const errorId = `${id}-error`;
  return (
    <AuthLayout>
      <PageTitle>{t('login.title')}</PageTitle>
      <p className="lede">{t('login.lede')}</p>
      <form className="form" onSubmit={submit} noValidate>
        {invalid && (
          <p id={errorId} className="alert" role="alert">
            {errorMessage(error)}
          </p>
        )}
        <div className="field">
          <label htmlFor={`${id}-email`}>{t('login.email')}</label>
          <input
            id={`${id}-email`}
            type="email"
            autoComplete="username"
            inputMode="email"
            spellCheck={false}
            required
            value={email}
            aria-invalid={invalid || undefined}
            aria-describedby={invalid ? errorId : undefined}
            onChange={(event) => setEmail(event.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor={`${id}-password`}>{t('login.password')}</label>
          <input
            id={`${id}-password`}
            type="password"
            autoComplete="current-password"
            required
            value={password}
            aria-invalid={invalid || undefined}
            aria-describedby={invalid ? errorId : undefined}
            onChange={(event) => setPassword(event.target.value)}
          />
        </div>
        <div className="form-actions">
          <button type="submit" className="button button-primary" aria-disabled={busy}>
            {busy ? t('login.submitting') : t('login.submit')}
          </button>
          <Link to="/reset" className="link">
            {t('login.forgot')}
          </Link>
        </div>
      </form>
    </AuthLayout>
  );
}
