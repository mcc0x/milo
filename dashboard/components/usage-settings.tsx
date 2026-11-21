'use client';

import { ChoiceSetting, GroupCard, SettingSection, SwitchSetting } from './controls';
import { useSession } from './session';
import type { UsageChoice } from './session-state';

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

const usageTools = [
  ['codex', 'Codex usage', toolDescriptions],
  ['claude', 'Claude Code usage', toolDescriptions],
  ['opencode', 'OpenCode usage', toolDescriptions],
] as const;

const usageAccounts = [
  ['deepseek', 'DeepSeek balance', accountDescriptions],
  ['openrouter', 'OpenRouter spend', accountDescriptions],
  ['devin', 'Devin ACUs', accountDescriptions],
  ['copilot', 'Copilot AI credits', accountDescriptions],
] as const;

export default function UsageSettings() {
  const { state, draftValue, edit } = useSession();
  const disabled = !state.groups.preferences.editable;

  function renderSetting([setting, label, descriptions]: (typeof usageTools | typeof usageAccounts)[number]) {
    const field = `appearance.usage_${setting}`;
    const value = draftValue<UsageChoice>(field);
    return <ChoiceSetting key={field} field={field} label={label} detail={descriptions[value]} value={value}
      choices={usageChoices} disabled={disabled} onChange={next => edit(field, next)} />;
  }

  return (
    <>
      <GroupCard group="preferences" title="Usage pane">
        <ChoiceSetting field="appearance.usage_large_figures" label="Usage display" detail="Auto fits the available space."
          value={draftValue<boolean>('appearance.usage_large_figures')}
          choices={figures} disabled={disabled} onChange={value => edit('appearance.usage_large_figures', value)} />
        <SettingSection id="usage-tools" title="Tools">
          {usageTools.map(renderSetting)}
        </SettingSection>
        <SettingSection id="usage-accounts" title="Provider accounts">
          {usageAccounts.map(renderSetting)}
        </SettingSection>
      </GroupCard>
      <GroupCard group="account" title="Claude Code">
        <SwitchSetting field="account.claude_limits" label="Show Claude Code limits" detail={state.account.claude_status}
          checked={draftValue<boolean>('account.claude_limits')} disabled={!state.groups.account.editable}
          onChange={checked => edit('account.claude_limits', checked)} />
      </GroupCard>
    </>
  );
}
