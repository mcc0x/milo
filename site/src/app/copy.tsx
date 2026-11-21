'use client';

import { createContext, use, useEffect, useRef, useState, type ReactNode } from 'react';

// Both Copy buttons report through the page's one status line.
const Announce = createContext<(message: string) => void>(() => {});
const Announcement = createContext('');

export function CopyAnnouncer({ children }: { children: ReactNode }) {
  const [message, setMessage] = useState('');
  return <Announce value={setMessage}><Announcement value={message}>{children}</Announcement></Announce>;
}

/** Tells a screen reader what the last Copy button did. */
export function CopyStatus() {
  return <p id="copy-feedback" className="visually-hidden" role="status">{use(Announcement)}</p>;
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
