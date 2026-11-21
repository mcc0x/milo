'use client';

import { useLayoutEffect, useRef, useState } from 'react';
import Alerts from './alerts';
import { GroupCard, SwitchSetting } from './controls';
import Presence from './presence';
import { useSession } from './session';
import './integrations.css';

const names = {
  automation: 'Rex automation',
  presence: 'Discord presence',
  alerts: 'Alert forwarding',
} as const;

type Detail = keyof typeof names;

function RexAutomation() {
  const { state, draftValue, edit } = useSession();
  return (
    <>
      <GroupCard group="preferences" title="Tab titles">
        <SwitchSetting field="workflow.tab_titles" label="Title Rex tabs" detail="Unnamed tabs only: project, worktree and agent."
          checked={draftValue<boolean>('workflow.tab_titles')} disabled={!state.groups.preferences.editable}
          onChange={checked => edit('workflow.tab_titles', checked)} />
      </GroupCard>
      {state.capabilities.auto_arrange ? (
        <GroupCard group="layout" title="New tab layout">
          <SwitchSetting field="layout.auto_arrange" label="Arrange new shell tabs"
            detail="This switch is global. New command tabs and tabs with an existing layout are skipped."
            checked={draftValue<boolean>('layout.auto_arrange')} onChange={checked => edit('layout.auto_arrange', checked)} />
        </GroupCard>
      ) : null}
    </>
  );
}

const unavailable = 'Not available in this session';
const alertDestinations = { telegram: 'Telegram', discord: 'Discord' } as const;

export default function Integrations() {
  const { state, draftValue, command } = useSession();
  const [detail, setDetail] = useState<Detail | null>(null);
  const returnTo = useRef<Detail | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const { presence, alerts, capabilities } = state;
  const available = { automation: true, presence: presence !== null, alerts: alerts !== null };

  useLayoutEffect(() => {
    if (detail) {
      heading.current?.focus();
    } else if (returnTo.current) {
      const button = document.getElementById(`integration-${returnTo.current}`);
      returnTo.current = null;
      if (button && !button.matches(':disabled')) button.focus();
      else heading.current?.focus();
    }
  }, [detail]);

  function open(next: Detail) {
    if (!available[next]) return;
    returnTo.current = next;
    setDetail(next);
    if (next === 'alerts') command('alerts', { field: 'refresh', value: null });
  }

  const autoArrange = capabilities.auto_arrange
    ? `Auto-arrange ${draftValue<boolean>('layout.auto_arrange') ? 'on' : 'off'}`
    : 'Auto-arrange unavailable';
  const summaries = {
    automation: `Tab titles ${draftValue<boolean>('workflow.tab_titles') ? 'on' : 'off'} · ${autoArrange}`,
    presence: presence ? (presence.enabled ? `On · ${presence.status}` : 'Off') : unavailable,
    alerts: alerts ? `${!alerts.enabled ? 'Off' : alerts.running ? 'On' : 'On, but not running'} · ${
      alerts.destination ? alertDestinations[alerts.destination] : 'No destination'}` : unavailable,
  };
  const immediate = detail && detail !== 'automation' && available[detail];

  return (
    <div className="integrations">
      <header className="integration-heading">
        <button type="button" className="integration-back" aria-label="Back to integrations" title="Back to integrations"
          disabled={!detail} onClick={() => setDetail(null)}>
          <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="2"
            strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M19 12H5m6-6-6 6 6 6"/></svg>
        </button>
          <h2 id="integrations-title" tabIndex={-1} ref={heading}
            aria-describedby={immediate ? 'integration-timing' : undefined}>{detail ? names[detail] : 'Integrations'}</h2>
        {immediate ? <p className="setting-detail" id="integration-timing">Applies immediately</p> : null}
      </header>
      {!detail ? (
        <ul className="integrations-list" role="list">
          {(Object.keys(names) as Detail[]).map(key => (
            <li key={key}>
              <button type="button" className="integration-row" id={`integration-${key}`} aria-label={names[key]}
                aria-describedby={`integration-${key}-summary`} disabled={!available[key]} onClick={() => open(key)}>
                <span className="setting-info">
                  <span className="setting-label">{names[key]}</span>
                  <span className="setting-detail" id={`integration-${key}-summary`}>{summaries[key]}</span>
                </span>
                {available[key] ? <span className="integration-chevron" aria-hidden="true">›</span> : null}
              </button>
            </li>
          ))}
        </ul>
      ) : !available[detail] ? (
        <p className="setting-detail">{unavailable}</p>
      ) : detail === 'automation' ? (
        <RexAutomation />
      ) : detail === 'presence' ? <Presence /> : <Alerts />}
    </div>
  );
}
