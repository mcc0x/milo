'use client';

import { createContext, use, useEffect, useRef, useState, type ReactNode } from 'react';

const Announce = createContext<(message: string) => void>(() => {});
const Announcement = createContext({ message: '', count: 0 });

export function CopyAnnouncer({ children }: { children: ReactNode }) {
  const [announcement, setAnnouncement] = useState({ message: '', count: 0 });
  const [announce] = useState(() => (message: string) => setAnnouncement(({ count }) => ({ message, count: count + 1 })));
  return <Announce value={announce}><Announcement value={announcement}>{children}</Announcement></Announce>;
}

/** Tells a screen reader what the last Copy button did. */
export function CopyStatus() {
  const { message, count } = use(Announcement);
  // A new key writes the text again, so copying twice is announced twice. The
  // text has an element of its own because page translation rewrites bare
  // text nodes, and React cannot replace one it no longer finds.
  return <p id="copy-feedback" className="visually-hidden" role="status"><span key={count}>{message}</span></p>;
}

/** Copies the text of the element it names, or selects it when the clipboard is unavailable. */
export function CopyButton({ target, id }: { target: string; id?: string }) {
  const announce = use(Announce);
  const [state, setState] = useState<'copied' | 'selected' | 'failed'>();
  const reset = useRef(0);
  useEffect(() => () => clearTimeout(reset.current), []);

  async function copy() {
    const command = document.getElementById(target)!;
    try {
      await navigator.clipboard.writeText(command.textContent);
      setState('copied');
      announce('Copied to clipboard.');
    } catch {
      const selection = window.getSelection();
      selection?.selectAllChildren(command);
      const selected = selection?.toString() === command.textContent;
      setState(selected ? 'selected' : 'failed');
      announce(selected
        ? 'Clipboard unavailable. Copy the selected command.'
        : 'Clipboard unavailable. Select and copy the command manually.');
    }
    clearTimeout(reset.current);
    reset.current = window.setTimeout(setState, 1800, undefined);
  }

  return (
    <button id={id} type="button" data-copy={target} aria-describedby={target} data-state={state === 'selected' ? 'failed' : state} onClick={copy}>
      {state === 'copied' ? 'Copied' : state === 'selected' ? 'Selected' : state === 'failed' ? 'Failed' : 'Copy'}
    </button>
  );
}
