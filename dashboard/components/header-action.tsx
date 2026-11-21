import type { ReactElement } from 'react';

type Props = { label: string; onClick: () => void; popup?: 'dialog' } & (
  { mode: 'text'; icon?: never } | { mode: 'icon' | 'text-icon'; icon: ReactElement }
);

export default function HeaderAction({ label, mode, icon, onClick, popup }: Props) {
  return <button type="button" className="header-action" aria-haspopup={popup} onClick={onClick}
    aria-label={mode === 'icon' ? label : undefined} title={mode === 'icon' ? label : undefined}>
    {mode !== 'text' && <span aria-hidden="true">{icon}</span>}
    {mode !== 'icon' && label}
  </button>;
}
