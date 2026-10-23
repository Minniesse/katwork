// Sets data-theme and lang before the first paint. Loaded synchronously from <head> as a file,
// because the server's CSP (default-src 'self') forbids inline scripts. Keep in step with
// src/theme.ts and src/i18n/index.ts.
(function () {
  var root = document.documentElement;
  var theme = null;
  var locale = null;
  try {
    theme = window.localStorage.getItem('kw-theme');
    locale = window.localStorage.getItem('kw-locale');
  } catch (error) {
    // Storage is blocked (privacy settings); use the system preference.
  }
  if (theme !== 'light' && theme !== 'dark') {
    var dark = window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches;
    theme = dark ? 'dark' : 'light';
  }
  if (locale !== 'en' && locale !== 'th') {
    locale = /^th\b/i.test(navigator.language || '') ? 'th' : 'en';
  }
  root.setAttribute('data-theme', theme);
  root.setAttribute('lang', locale);
})();
