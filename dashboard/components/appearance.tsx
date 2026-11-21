'use client';

import { ChoiceSetting, GroupCard } from './controls';
import { useSession } from './session';
import type { UsageChoice } from './session-state';

const icons = [['nerd', 'Nerd Font'], ['plain', 'Plain'], ['off', 'Off']] as const;
const marks = [[false, 'Pulse'], [true, 'Static']] as const;
const gitMarks = [[false, 'Style only', 'main.rs'], [true, 'Text', 'M main.rs']] as const;
const figures = [[false, 'Auto compact'], [true, 'Large figures']] as const;
const usageChoices = [['auto', 'Auto'], ['always', 'Always show'], ['hide', 'Hide']] as const;

const toolDescriptions = {
  auto: 'Shown while it has a figure saved on this Mac.',
  always: 'Shown even when nothing is known about it.',
  hide: 'Never shown, and never looked for.',
};

// Accounts are asked about over the network, so the server defaults each to hidden.
const accountDescriptions = {
  auto: 'Asks the provider with the key you set for Milo. Shown once it answers. Not a limit.',
  always: 'Asks the provider, and says why when there is no figure. Not a limit.',
  hide: 'Never shown, and the provider is never contacted.',
};

const usageSettings = [
  ['codex', 'Codex usage', toolDescriptions],
  ['claude', 'Claude Code usage', toolDescriptions],
  ['opencode', 'OpenCode usage', toolDescriptions],
  ['deepseek', 'DeepSeek balance', accountDescriptions],
  ['openrouter', 'OpenRouter spend', accountDescriptions],
  ['devin', 'Devin ACUs', accountDescriptions],
  ['copilot', 'Copilot AI credits', accountDescriptions],
] as const;

export default function Appearance() {
  const { state, draftValue, edit } = useSession();
  const disabled = !state.groups.preferences.editable;

  return (
    <GroupCard group="preferences" title="Pane appearance">
      <ChoiceSetting field="appearance.icons" label="Icons" value={draftValue<'nerd' | 'plain' | 'off'>('appearance.icons')}
        choices={icons} disabled={disabled} onChange={value => edit('appearance.icons', value)} />
      <ChoiceSetting field="appearance.static_marks" label="Activity marks" value={draftValue<boolean>('appearance.static_marks')}
        choices={marks} disabled={disabled} onChange={value => edit('appearance.static_marks', value)} />
      <ChoiceSetting field="appearance.text_git_marks" label="Git status marks" value={draftValue<boolean>('appearance.text_git_marks')}
        choices={gitMarks} disabled={disabled} onChange={value => edit('appearance.text_git_marks', value)} />
      <ChoiceSetting field="appearance.usage_large_figures" label="Usage display" detail="Auto fits the available space."
        value={draftValue<boolean>('appearance.usage_large_figures')}
        choices={figures} disabled={disabled} onChange={value => edit('appearance.usage_large_figures', value)} />
      {usageSettings.map(([setting, label, descriptions]) => {
        const field = `appearance.usage_${setting}`;
        const value = draftValue<UsageChoice>(field);
        return <ChoiceSetting key={field} field={field} label={label} detail={descriptions[value]} value={value}
          choices={usageChoices} disabled={disabled} onChange={next => edit(field, next)} />;
      })}
    </GroupCard>
  );
}
