'use client';

import { ChoiceSetting, Disclosure, GroupCard, NumberInput } from './controls';
import { useSession } from './session';
import type { PaneRole } from './session-state';

const roleNames = { outline: 'Outline', history: 'History', usage: 'Usage' };

const directions = [[-1, '←', 'up', 'earlier'], [1, '→', 'down', 'later']] as const;
const sides = [['left', 'Left'], ['right', 'Right']] as const;

function PanePicker({ disabled }: { disabled: boolean }) {
  const { draftValue, edit } = useSession();
  const order = draftValue<PaneRole[]>('layout.order');

  return (
    <fieldset className="pane-picker" disabled={disabled}>
      <legend className="sr-only">Panes in rail order</legend>
      <ol className="pane-list">
        {order.map((role, index) => {
          const field = `layout.panes.${role}`;
          return (
            <li className="pane-line" key={role}>
              <label>
                <input id={`enable-${role}`} type="checkbox" checked={draftValue<boolean>(field)}
                  onChange={event => edit(field, event.currentTarget.checked)} />{roleNames[role]}
              </label>
              <div className="button-row">
                {directions.map(([delta, arrow, direction, description]) => (
                  <button key={direction} type="button" className="button secondary" id={`move-${role}-${direction}`}
                    aria-label={`Move ${roleNames[role]} ${description}`}
                    disabled={index + delta < 0 || index + delta >= order.length}
                    onClick={() => {
                      const next = draftValue<PaneRole[]>('layout.order').slice();
                      const from = next.indexOf(role);
                      const to = from + delta;
                      if (to < 0 || to >= next.length) return;
                      [next[from], next[to]] = [next[to], next[from]];
                      edit('layout.order', next);
                    }}>{arrow}</button>
                ))}
              </div>
            </li>
          );
        })}
      </ol>
    </fieldset>
  );
}

function LayoutActions() {
  const { state, paused, numericDraftCount, queue, confirm, command, edit } = useSession();
  const { groups, layout, capabilities, trial } = state;
  const resetHeights = groups.layout.editable && layout.ratios;
  const undo = trial && capabilities.undo;
  if (!resetHeights && !undo && !capabilities.try && !capabilities.capture) return null;

  return (
    <Disclosure id="layout-actions" label="•••" accessibleName="Layout actions">
      {resetHeights ? (
        <button type="button" className="button text-button" id="reset-heights" disabled={paused}
          onClick={() => edit('layout.ratios', null)}>Use automatic heights</button>
      ) : null}
      {undo ? (
        <button type="button" className="button text-button" id="undo-layout" disabled={paused}
          onClick={() => command('undo')}>Undo trial</button>
      ) : capabilities.try ? (
        <button type="button" className="button text-button" id="try-layout" disabled={paused}
          onClick={() => command('try')}>Try in Rex</button>
      ) : null}
      {capabilities.capture ? (
        <button type="button" className="button text-button" id="capture" disabled={paused} onClick={() => {
          if (groups.layout.dirty || numericDraftCount || queue.some(item => item.field?.startsWith('layout.'))) {
            confirm('Replace the layout draft?', 'Copy the side, order and pane sizes from your Rex tab without saving.',
              'Copy layout', () => command('capture'));
          } else command('capture');
        }}>Copy layout from Rex tab…</button>
      ) : null}
    </Disclosure>
  );
}

export default function LayoutSettings() {
  const session = useSession();
  const { state, draftValue, edit } = session;
  const disabled = !state.groups.layout.editable;

  return (
    <GroupCard group="layout" title="Pane layout" actions={<LayoutActions />}>
      <PanePicker disabled={disabled} />
      <ChoiceSetting field="layout.side" label="Rail side" value={draftValue<'left' | 'right'>('layout.side')}
        choices={sides} disabled={disabled} onChange={value => edit('layout.side', value)} />
      <NumberInput field="layout.width" label="Rail width" detail="Percentage of the tab." percent
        value={draftValue<number>('layout.width')} drafts={session} min={0.000001} max={99.999999} step="any" disabled={disabled} />
      <NumberInput field="layout.usage_rows" label="Usage height" detail="Rows. Changing this restores automatic pane heights."
        value={draftValue<number>('layout.usage_rows')} drafts={session} min={1} max={4294967295} step={1} disabled={disabled} />
    </GroupCard>
  );
}
