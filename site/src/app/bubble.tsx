'use client';

import { useEffect, useRef, type PointerEvent, type ReactNode } from 'react';

/** An integration's glass bubble. Its highlight follows a mouse across it. */
export function Bubble({ children }: { children: ReactNode }) {
  const bubble = useRef<HTMLLIElement>(null);
  const frame = useRef(0);
  const pointer = useRef({ x: 0, y: 0 });
  useEffect(() => () => cancelAnimationFrame(frame.current), []);

  // The highlight is written straight to the element: it changes every frame
  // and nothing else on the page depends on it.
  function follow(event: PointerEvent) {
    if (event.pointerType !== 'mouse' || matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    pointer.current = { x: event.clientX, y: event.clientY };
    if (frame.current) return;
    frame.current = requestAnimationFrame(() => {
      frame.current = 0;
      const box = bubble.current?.getBoundingClientRect();
      if (!box || box.width === 0 || box.height === 0) return;
      bubble.current!.style.setProperty('--glint-x', `${20 + 60 * (pointer.current.x - box.left) / box.width}%`);
      bubble.current!.style.setProperty('--glint-y', `${15 + 50 * (pointer.current.y - box.top) / box.height}%`);
    });
  }

  function rest() {
    cancelAnimationFrame(frame.current);
    frame.current = 0;
    bubble.current!.style.removeProperty('--glint-x');
    bubble.current!.style.removeProperty('--glint-y');
  }

  return <li ref={bubble} onPointerMove={follow} onPointerLeave={rest}><span>{children}</span></li>;
}
