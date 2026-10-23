import {
  useEffect,
  useState,
  useSyncExternalStore,
  type AnchorHTMLAttributes,
  type MouseEvent,
} from 'react';

// A small history-API router; the server answers every non-API path with index.html.

const listeners = new Set<() => void>();
let navigations = 0;

/** How many in-app navigations have happened; 0 on the first page load. */
export function navigationCount(): number {
  return navigations;
}

const onPopState = () => {
  navigations += 1;
};

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  window.addEventListener('popstate', onPopState);
  window.addEventListener('popstate', listener);
  return () => {
    listeners.delete(listener);
    window.removeEventListener('popstate', listener);
  };
}

// The count makes a navigation to the same path (a new history entry) re-render too.
const snapshot = () => `${navigations} ${window.location.pathname}${window.location.search}`;

export function navigate(
  to: string,
  { replace = false, state = null }: { replace?: boolean; state?: unknown } = {},
): void {
  if (replace) window.history.replaceState(state, '', to);
  else window.history.pushState(state, '', to);
  navigations += 1;
  listeners.forEach((listener) => listener());
}

/** The current path and query, re-rendering on navigation. */
export function useLocation(): URL {
  const current = useSyncExternalStore(subscribe, snapshot);
  return new URL(current.slice(current.indexOf(' ') + 1), window.location.origin);
}

/**
 * The `?token=` of the emailed or printed link this page was opened with, for a page at
 * `path`. It is read once into memory and removed from the address bar with a replace that
 * leaves no history state, so neither the URL nor `history.state` holds it. It is dropped on
 * the next navigation of any kind, so Back to this entry, or a reload, finds no token. The
 * browser's global history can still list the original link; single use and expiry bound that.
 */
export function useLinkToken(path: string): string | null {
  const [link, setLink] = useState(() => {
    if (window.location.pathname !== path) return null;
    const token = new URLSearchParams(window.location.search).get('token');
    return token ? { token, opened: navigations } : null;
  });
  const count = useSyncExternalStore(subscribe, navigationCount);
  useEffect(() => {
    // Only while the token is still in the address bar, so a repeated effect never navigates twice.
    if (window.location.pathname === path && new URLSearchParams(window.location.search).has('token')) {
      navigate(path, { replace: true });
    }
  }, [path]);
  // Removing the token is one navigation; any later one leaves the page the link opened.
  const current = link !== null && count <= link.opened + 1;
  useEffect(() => {
    if (link !== null && !current) setLink(null);
  }, [link, current]);
  return current ? link.token : null;
}

export function Link({
  to,
  onClick,
  ...props
}: { to: string } & AnchorHTMLAttributes<HTMLAnchorElement>) {
  const follow = (event: MouseEvent<HTMLAnchorElement>) => {
    onClick?.(event);
    if (
      event.defaultPrevented ||
      event.button !== 0 ||
      event.metaKey ||
      event.ctrlKey ||
      event.shiftKey ||
      event.altKey
    ) {
      return;
    }
    event.preventDefault();
    navigate(to);
  };
  return <a href={to} onClick={follow} {...props} />;
}
