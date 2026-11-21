'use client';

import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { useSession } from './session';
import { groupLabels } from './session-state';
import type { GroupKey, GroupState, Scope } from './session-state';

export function SettingRow({ label, htmlFor, detail, detailId, detailClassName = 'setting-detail', messages, children }: {
  label: string;
  htmlFor?: string;
  detail?: string;
  detailId?: string;
  detailClassName?: string;
  messages?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div className="setting">
      <div className="setting-info">
        {htmlFor ? <label className="setting-label" htmlFor={htmlFor}>{label}</label>
          : <span className="setting-label">{label}</span>}
        {detail ? <span className={detailClassName} id={detailId}>{detail}</span> : null}
        {messages}
      </div>
      {children}
    </div>
  );
}

export function Toggle({ id, checked, disabled, label, 'aria-describedby': describedBy, onChange }: {
  id: string;
  checked: boolean;
  disabled?: boolean;
  label?: string;
  'aria-describedby'?: string;
  onChange: (checked: boolean) => void;
}) {
  return (
    <div className="setting-input">
      <input id={id} className="switch" type="checkbox" role="switch" checked={checked} disabled={disabled}
        aria-label={label} aria-describedby={describedBy} onChange={event => onChange(event.currentTarget.checked)} />
      <span className="switch-track" aria-hidden="true" />
    </div>
  );
}

export function SwitchSetting({ field, label, detail, checked, disabled, onChange }: {
  field: string;
  label: string;
  detail?: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (checked: boolean) => void;
}) {
  const id = `field-${field}`;
  const detailId = detail ? `${id}-detail` : undefined;
  return (
    <SettingRow label={label} htmlFor={id} detail={detail} detailId={detailId}>
      <Toggle id={id} checked={checked} disabled={disabled} aria-describedby={detailId} onChange={onChange} />
    </SettingRow>
  );
}

type Choice = string | boolean;

export function ChoiceSetting<T extends Choice>({ field, label, detail, disabled, value, choices, onChange }: {
  field: string;
  label: string;
  detail?: string;
  disabled?: boolean;
  value: T;
  choices: readonly (readonly [key: T, text: string, example?: string])[];
  onChange: (value: T) => void;
}) {
  const detailId = detail ? `${field}-detail` : undefined;
  return (
    <SettingRow label={label} detail={detail} detailId={detailId}>
      <fieldset className="choice-strip" disabled={disabled} aria-describedby={detailId}>
        <legend className="sr-only">{label}</legend>
        {choices.map(([key, text, example]) => (
          <label key={String(key)}>
            <input type="radio" name={field} id={`field-${field}-${key}`} checked={key === value}
              onChange={() => onChange(key)} />
            <span>{text}{example ? <code className="git-example" aria-hidden="true">{example}</code> : null}</span>
          </label>
        ))}
      </fieldset>
    </SettingRow>
  );
}

interface NumericDrafts {
  trackNumericDraft(field: string, control: HTMLInputElement, scale: number): void;
  commitNumericDraft(field: string): void;
  applyNumericDraft(field: string): void;
  hasNumericDraft(field: string): boolean;
}

export function NumberInput({ field, label, detail, value, drafts, min, max, step, percent = false, disabled }: {
  field: string;
  label: string;
  detail?: string;
  value: number;
  drafts: NumericDrafts;
  min: number;
  max: number;
  step: number | 'any';
  percent?: boolean;
  disabled?: boolean;
}) {
  const id = `field-${field}`;
  const detailId = `${id}-detail`;
  const errorId = `${id}-error`;
  const scale = percent ? 100 : 1;
  const initial = percent ? Number((value * scale).toFixed(6)) : value;
  const startingValue = useRef(initial);
  const input = useRef<HTMLInputElement>(null);
  const [error, setError] = useState('');
  const { trackNumericDraft, commitNumericDraft, applyNumericDraft, hasNumericDraft } = drafts;
  const draft = hasNumericDraft(field);

  useLayoutEffect(() => {
    const control = input.current;
    if (!control) return;
    control.dataset.initial = String(initial);
    if (!draft && control.valueAsNumber !== initial) {
      control.value = String(initial);
      setError('');
    }
  }, [draft, initial]);

  useEffect(() => {
    const control = input.current;
    if (!control) return;
    // React's onChange fires while typing; the native change event commits on blur.
    const commit = () => commitNumericDraft(field);
    control.addEventListener('change', commit);
    return () => control.removeEventListener('change', commit);
  }, [commitNumericDraft, field]);

  return (
    <SettingRow label={label} htmlFor={id} detail={detail} detailId={detailId}
      messages={<span id={errorId} aria-live="polite" className={error ? 'setting-detail' : undefined}>{error}</span>}>
      <div className="setting-input">
        <input ref={input} id={id} type="number" min={min} max={max} step={step} required disabled={disabled}
          defaultValue={startingValue.current} aria-describedby={detail ? `${detailId} ${errorId}` : errorId}
          aria-invalid={error ? true : undefined}
          onInput={event => {
            const control = event.currentTarget;
            if (control.validity.valid) setError('');
            trackNumericDraft(field, control, scale);
          }}
          onBlur={event => setError(event.currentTarget.validationMessage)}
          onInvalid={event => setError(event.currentTarget.validationMessage)}
          onKeyDown={event => {
            if (event.key !== 'Enter' || event.nativeEvent.isComposing) return;
            event.preventDefault();
            if (event.currentTarget.reportValidity()) applyNumericDraft(field);
          }} />
        {percent ? <span className="unit">%</span> : null}
      </div>
    </SettingRow>
  );
}

export function Disclosure({ id, label, accessibleName, children }: {
  id: string;
  label: string;
  accessibleName: string;
  children: ReactNode;
}) {
  const menu = useRef<HTMLDetailsElement>(null);
  const trigger = useRef<HTMLElement>(null);

  useEffect(() => {
    function outsidePress(event: PointerEvent) {
      const details = menu.current;
      if (details?.open && event.target instanceof Node && !details.contains(event.target)) details.open = false;
    }
    function escape(event: KeyboardEvent) {
      const details = menu.current;
      if (!details?.open || event.key !== 'Escape' || event.defaultPrevented || document.querySelector('dialog[open]')) return;
      if (details.contains(document.activeElement)) trigger.current?.focus();
      details.open = false;
      event.preventDefault();
    }
    document.addEventListener('pointerdown', outsidePress);
    document.addEventListener('keydown', escape);
    return () => {
      document.removeEventListener('pointerdown', outsidePress);
      document.removeEventListener('keydown', escape);
    };
  }, []);

  return (
    <details ref={menu} className="action-menu" name="editor-actions">
      <summary ref={trigger} id={id} aria-label={accessibleName}><span>{label}</span></summary>
      <div className="action-menu-items" onClickCapture={event => {
        if (!(event.target instanceof Element) || !event.target.closest('button:not(:disabled)')) return;
        // A dialog opened by the action must remember the summary, not a hidden action.
        trigger.current?.focus();
        if (menu.current) menu.current.open = false;
      }}>
        {children}
      </div>
    </details>
  );
}

const saveScopes = [
  ['worktree', 'Save for this worktree'],
  ['global', 'Save as global default'],
  ['account', 'Save to active Claude profile'],
] as const;

export function SaveChoices({ group, available, select, onSave, disabled }: {
  group: GroupKey;
  available: GroupState;
  select: 'dock' | 'install';
  onSave: (scope: Scope) => void;
  disabled?: boolean;
}) {
  return (
    <div className="button-row">
      {saveScopes.filter(([scope]) => available[`save_${scope}`]).map(([scope, label], index) => (
        <button key={scope} type="button" className={`button ${index === 0 ? 'primary' : 'secondary'}`}
          id={`${select}-${group}-${scope}`} disabled={disabled} onClick={() => onSave(scope)}>{label}</button>
      ))}
    </div>
  );
}

const originLabels = {
  default: 'Built-in defaults',
  global: 'Global default',
  worktree: 'Worktree override',
  account: 'Account settings',
};

export function GroupCard({ group, title, actions, children }: {
  group: GroupKey;
  title: string;
  actions?: ReactNode;
  children: ReactNode;
}) {
  const { state, paused, numericDraftCount, confirm, command } = useSession();
  const settings = state.groups[group];
  const origin = originLabels[settings.scope];
  const id = `group-${group}`;

  return (
    <section className="settings-group" aria-labelledby={id}>
      <header className="group-header">
        <div className="group-titleline">
          <h2 id={id} tabIndex={-1}>{title}</h2>
          <div className="scope-actions">
            {settings.inherit ? (
              <Disclosure id={`scope-${group}`} label={`${origin} ▾`} accessibleName={`${origin} options`}>
                <button type="button" className="button text-button" id={`inherit-${group}`} disabled={paused}
                  onClick={() => {
                    const dirty = settings.dirty || (group === 'layout' && numericDraftCount > 0);
                    confirm('Remove worktree override?',
                      `This worktree will use the global ${groupLabels[group]}${dirty ? ' and lose unsaved changes in this group' : ''}.`,
                      'Remove override', () => command('inherit', { group }));
                  }}>Remove override…</button>
              </Disclosure>
            ) : <span className="group-origin">{origin}</span>}
            {group === 'layout' && state.context.inside_rex && !state.context.installed &&
              (settings.save_global || settings.save_worktree) ? (
                <Disclosure id="setup-layout" label="Set up in Rex…" accessibleName="Set up in Rex">
                  <SaveChoices group={group} available={settings} select="install" disabled={paused}
                    onSave={scope => command('save', { group, scope })} />
                </Disclosure>
              ) : null}
            {actions}
          </div>
        </div>
      </header>
      <div className="group-controls">{children}</div>
      {!settings.editable ? <p className="setting-note">Read-only in this session.</p> : null}
    </section>
  );
}

export function SettingSection({ id, title, children }: {
  id: string;
  title: string;
  children: ReactNode;
}) {
  return (
    <section aria-labelledby={id}>
      <h3 id={id} className="setting-subheading">{title}</h3>
      <div className="group-controls">{children}</div>
    </section>
  );
}
