import { useEffect, useRef, type ReactNode } from 'react';

/**
 * A confirmation that replaces a form. It takes focus when it appears, so a screen reader
 * reads it even though the status region is new.
 */
export function Announce({ children }: { children: ReactNode }) {
  const region = useRef<HTMLDivElement>(null);
  useEffect(() => region.current?.focus(), []);
  return (
    <div ref={region} className="notice" role="status" tabIndex={-1}>
      {children}
    </div>
  );
}
