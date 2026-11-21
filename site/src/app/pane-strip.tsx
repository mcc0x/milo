'use client';

import { useEffect, useRef, useState, type ReactNode } from 'react';

/** The four panes. Their icons are complete in the HTML and play an entrance the first time they scroll into view. */
export function PaneStrip({ children }: { children: ReactNode }) {
  const strip = useRef<HTMLElement>(null);
  const [beat, setBeat] = useState(false);
  useEffect(() => {
    const observer = new IntersectionObserver(([entry]) => {
      if (!entry.isIntersecting) return;
      if (!matchMedia('(prefers-reduced-motion: reduce)').matches) setBeat(true);
      observer.disconnect();
    }, { threshold: 0.25 });
    observer.observe(strip.current!);
    return () => observer.disconnect();
  }, []);
  return <article ref={strip} className={`feature feature-panes${beat ? ' pane-beat' : ''}`}>{children}</article>;
}
