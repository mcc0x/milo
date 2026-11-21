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

function IntegrationMark({kind}: {kind: Detail}) {
  return <span className="integration-mark" data-kind={kind} aria-hidden="true">
    {kind === 'automation' ? <img src="/rex.png" alt="" width="23" height="23"/> :
      <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        {kind === 'presence' ? <><path d="M4 4h16v12H9l-5 4z"/><path d="M8 10h2l1-2 2 4 1-2h2"/></> :
          <><path d="M3 6h9M3 11h6m-6 5h4m4-1 9-9m-7 0h7v7"/></>}
      </svg>}
  </span>;
}

function RexAutomation() {
  const { state, draftValue, edit } = useSession();
  return (
    <>
      <GroupCard group="preferences" title="Naming">
        <SwitchSetting field="workflow.tab_titles" label="Title Rex tabs" detail="Name unnamed tabs after the project, worktree, and active agent."
          checked={draftValue<boolean>('workflow.tab_titles')} disabled={!state.groups.preferences.editable}
          onChange={checked => edit('workflow.tab_titles', checked)} />
      </GroupCard>
      {state.capabilities.auto_arrange ? (
        <GroupCard group="layout" title="Arrangement">
          <SwitchSetting field="layout.auto_arrange" label="Arrange new shell tabs"
            detail="Applies globally to new shell tabs. Existing layouts and command tabs stay untouched."
            checked={draftValue<boolean>('layout.auto_arrange')} onChange={checked => edit('layout.auto_arrange', checked)} />
        </GroupCard>
      ) : <p className="setting-note">Automatic arrangement isn’t available in this session.</p>}
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

  const rules = Number(draftValue<boolean>('workflow.tab_titles')) + Number(draftValue<boolean>('layout.auto_arrange'));
  const summaries = {
    automation: rules ? `${rules} ${rules === 1 ? 'rule' : 'rules'} on` : 'Off',
    presence: presence ? (presence.enabled ? 'On' : 'Off') : unavailable,
    alerts: alerts ? (alerts.enabled ? (alerts.running ? 'On' : 'Not running') : 'Off') : unavailable,
  };
  const descriptions = {
    automation: 'Tab titles and new shell layouts',
    presence: 'What appears on your Discord profile',
    alerts: alerts?.destination ? `Destination: ${alertDestinations[alerts.destination]}` : 'Send Rex alerts to a chat or channel',
  };
  const timing = detail && available[detail] ? (detail === 'automation' ? 'Save to apply changes' : 'Applies immediately') : '';

  return (
    <div className="integrations">
      <header className="integration-heading">
        <button type="button" className="integration-back" aria-label="Back to integrations" title="Back to integrations"
          disabled={!detail} onClick={() => setDetail(null)}>
          <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="2"
            strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M19 12H5m6-6-6 6 6 6"/></svg>
        </button>
        <h2 id="integrations-title" tabIndex={-1} ref={heading}
          aria-describedby={timing ? 'integration-timing' : undefined}>{detail ? names[detail] : 'Integrations'}</h2>
        {timing ? <p className="setting-detail" id="integration-timing">{timing}</p> : null}
      </header>
      {!detail ? (
        <ul className="integrations-list" role="list">
          {(Object.keys(names) as Detail[]).map(key => (
            <li key={key}>
              <button type="button" className="integration-row" id={`integration-${key}`} aria-label={names[key]}
                aria-describedby={`integration-${key}-summary`} disabled={!available[key]} onClick={() => open(key)}>
                <IntegrationMark kind={key}/>
                <span className="setting-info">
                  <span className="setting-label">{names[key]}</span>
                  <span className="setting-detail">{descriptions[key]}</span>
                </span>
                <span className="integration-state" id={`integration-${key}-summary`}
                  data-state={!available[key] ? 'unavailable' : key === 'alerts' && alerts?.enabled && !alerts.running ? 'warning' :
                    (key === 'automation' ? rules > 0 : key === 'presence' ? presence?.enabled : alerts?.enabled) ? 'on' : 'off'}>
                  {summaries[key]}
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
