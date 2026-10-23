import { useId, useState, type FormEvent } from 'react';
import { ApiError, api, errorMessage } from '../api';
import { Announce } from '../components/Announce';
import { AuthLayout, PageTitle } from '../components/Layout';
import { t, useLocale } from '../i18n';
import { Link } from '../router';

/** The server answers 202 for every address, so the confirmation never depends on the email. */
export function ResetRequest() {
  useLocale();
  const id = useId();
  const [email, setEmail] = useState('');
  const [sent, setSent] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    if (!email.trim()) {
      setError(new ApiError(0, 'request_invalid'));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api.requestReset(email.trim());
      setSent(true);
    } catch (failure) {
      setError(failure);
    } finally {
      setBusy(false);
    }
  };

  return (
    <AuthLayout>
      <PageTitle>{t('reset.request.title')}</PageTitle>
      {sent ? (
        <Announce>
          <p>{t('reset.request.sent')}</p>
          <p>
            <Link to="/login" className="link">
              {t('reset.back_to_login')}
            </Link>
          </p>
        </Announce>
      ) : (
        <>
          <p className="lede">{t('reset.request.lede')}</p>
          <form className="form" onSubmit={submit} noValidate>
            {error !== null && (
              <p id={`${id}-error`} className="alert" role="alert">
                {errorMessage(error)}
              </p>
            )}
            <div className="field">
              <label htmlFor={`${id}-email`}>{t('reset.email')}</label>
              <input
                id={`${id}-email`}
                type="email"
                autoComplete="username"
                inputMode="email"
                spellCheck={false}
                required
                value={email}
                aria-invalid={error !== null || undefined}
                aria-describedby={error !== null ? `${id}-error` : undefined}
                onChange={(event) => setEmail(event.target.value)}
              />
            </div>
            <div className="form-actions">
              <button type="submit" className="button button-primary" aria-disabled={busy}>
                {busy ? t('reset.request.sending') : t('reset.request.submit')}
              </button>
              <Link to="/login" className="link">
                {t('reset.back_to_login')}
              </Link>
            </div>
          </form>
        </>
      )}
    </AuthLayout>
  );
}
