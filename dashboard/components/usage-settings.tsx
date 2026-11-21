'use client';

import { ChoiceSetting, GroupCard, SettingSection, SwitchSetting } from './controls';
import { useSession } from './session';
import type { UsageChoice } from './session-state';
import codex from './marks/codex.png';
import claude from './marks/claude.png';
import opencode from './marks/opencode.png';
import deepseek from './marks/deepseek.png';
import openrouter from './marks/openrouter.png';
import devin from './marks/devin.png';
import copilot from './marks/copilot.png';

const marks = {codex, claude, opencode, deepseek, openrouter, devin, copilot};

const figures = [[false, 'Auto compact'], [true, 'Large figures']] as const;
const usageChoices = [['auto', 'Auto'], ['always', 'Show'], ['hide', 'Hide']] as const;
const usageTools = [['codex', 'Codex'], ['claude', 'Claude Code'], ['opencode', 'OpenCode']] as const;
const usageAccounts = [
  ['deepseek', 'DeepSeek balance'], ['openrouter', 'OpenRouter spend'],
  ['devin', 'Devin ACUs'], ['copilot', 'Copilot AI credits'],
] as const;

export default function UsageSettings() {
  const { state, draftValue, edit } = useSession();
  const disabled = !state.groups.preferences.editable;

  const enabled = usageAccounts.filter(([key]) => draftValue<UsageChoice>(`appearance.usage_${key}`) !== 'hide').length;

  function renderSetting([setting, label]: (typeof usageTools | typeof usageAccounts)[number]) {
    const field = `appearance.usage_${setting}`;
    const value = draftValue<UsageChoice>(field);
    return <label className="usage-source" key={field} data-provider={setting}>
      <span className="usage-mark"><img src={marks[setting].src} alt="" width="24" height="24" /></span>
      <span>{label}</span>
      <select value={value} data-choice={value} disabled={disabled}
        aria-label={label} onChange={event => edit(field, event.currentTarget.value)}>
        {usageChoices.map(([key, text]) => <option key={key} value={key}>{text}</option>)}
      </select>
    </label>;
  }

  return (
    <div className="usage-settings">
      <GroupCard group="preferences" title="Usage pane">
        <ChoiceSetting field="appearance.usage_large_figures" label="Display"
          value={draftValue<boolean>('appearance.usage_large_figures')}
          choices={figures} disabled={disabled} onChange={value => edit('appearance.usage_large_figures', value)} />
        <SettingSection id="usage-tools" title="Tools">
          <p className="setting-detail">Auto shows a tool when local usage is available.</p>
          {usageTools.map(renderSetting)}
        </SettingSection>
        <details className="usage-accounts">
          <summary>Provider accounts <span>{enabled} enabled</span></summary>
          <p className="setting-detail">Auto and Show query each provider using your configured key. Auto waits for a figure; Show also reports missing data. Hide never contacts the provider.</p>
          <div className="group-controls">{usageAccounts.map(renderSetting)}</div>
        </details>
      </GroupCard>
      <GroupCard group="account" title="Claude Code">
        <SwitchSetting field="account.claude_limits" label="Show Claude Code limits" detail={state.account.claude_status}
          checked={draftValue<boolean>('account.claude_limits')} disabled={!state.groups.account.editable}
          onChange={checked => edit('account.claude_limits', checked)} />
      </GroupCard>
    </div>
  );
}
