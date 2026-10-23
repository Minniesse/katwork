import '@fontsource-variable/nunito';
import '@fontsource-variable/noto-sans-thai-looped';
import './theme.css';
import './app.css';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './app';
import { getLocale } from './i18n';

document.documentElement.lang = getLocale();

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
