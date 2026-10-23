import { LOCALES, setLocale, t, useLocale } from '../i18n';
import { setTheme, useTheme } from '../theme';

/** Language and dark-mode switches, shown on every page. */
export function Preferences() {
  const locale = useLocale();
  const theme = useTheme();
  return (
    <div className="preferences">
      <div className="segmented" role="group" aria-label={t('nav.language')}>
        {LOCALES.map((option) => (
          <button
            key={option}
            type="button"
            lang={option}
            aria-pressed={locale === option}
            onClick={() => setLocale(option)}
          >
            {t(`locale.${option}`)}
          </button>
        ))}
      </div>
      <button
        type="button"
        className="button button-quiet"
        aria-pressed={theme === 'dark'}
        onClick={() => setTheme(theme === 'dark' ? 'light' : 'dark')}
      >
        <svg className="icon" viewBox="0 0 20 20" aria-hidden="true" focusable="false">
          <path d="M15.5 12.6A6.5 6.5 0 0 1 7.4 4.5a6.5 6.5 0 1 0 8.1 8.1Z" />
        </svg>
        <span className="button-label">{t('nav.dark_mode')}</span>
      </button>
    </div>
  );
}
