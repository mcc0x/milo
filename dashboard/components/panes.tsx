'use client';

import { ChoiceSetting, GroupCard, SettingSection, SwitchSetting } from './controls';
import { useSession } from './session';
import PaneGlyph from './pane-glyph';
import type { SessionState } from './session-state';

const tracking = [['follow', 'Follow shell'], ['pin', 'Pin project']] as const;
const outline = [[false, 'All files'], [true, 'Changes only']] as const;
const placements = [['right', 'Right'], ['left', 'Left'], ['above', 'Above'], ['below', 'Below']] as const;
const icons = [['nerd', 'Nerd Font'], ['plain', 'Plain'], ['off', 'Off']] as const;
const marks = [[false, 'Pulse'], [true, 'Static']] as const;
const gitMarks = [[false, 'Style only', 'main.rs'], [true, 'Text', 'M main.rs']] as const;

export default function Panes() {
  const { state, draftValue, edit } = useSession();
  const disabled = !state.groups.preferences.editable;

  return (
    <GroupCard group="preferences" title="Pane preferences">
      <SettingSection id="pane-outline" title={<><PaneGlyph kind="outline"/>Outline</>}>
        <ChoiceSetting field="workflow.project_mode" label="Project tracking"
          value={draftValue<'follow' | 'pin'>('workflow.project_mode')} choices={tracking}
          disabled={disabled} onChange={value => edit('workflow.project_mode', value)} />
        <SwitchSetting field="workflow.follow_activity" label="Follow file edits" detail="Temporarily reveal active files."
          checked={draftValue<boolean>('workflow.follow_activity')}
          disabled={disabled} onChange={checked => edit('workflow.follow_activity', checked)} />
        <ChoiceSetting field="workflow.changes_only" label="Show on open"
          value={draftValue<boolean>('workflow.changes_only')} choices={outline}
          disabled={disabled} onChange={value => edit('workflow.changes_only', value)} />
      </SettingSection>
      <SettingSection id="pane-history" title={<><PaneGlyph kind="history"/>History</>}>
        <SwitchSetting field="workflow.history_follow_new" label="Follow new commits" detail="Only while History is at the top."
          checked={draftValue<boolean>('workflow.history_follow_new')}
          disabled={disabled} onChange={checked => edit('workflow.history_follow_new', checked)} />
      </SettingSection>
      <SettingSection id="pane-preview" title={<><PaneGlyph kind="preview"/>Preview</>}>
        <ChoiceSetting field="workflow.preview_placement" label="Preview placement"
          detail="Where new Preview tiles open relative to the working terminal."
          value={draftValue<SessionState['workflow']['preview_placement']>('workflow.preview_placement')} choices={placements}
          disabled={disabled} onChange={value => edit('workflow.preview_placement', value)} />
      </SettingSection>
      <SettingSection id="pane-appearance" title={<><PaneGlyph kind="appearance"/>Shared appearance</>}>
        <ChoiceSetting field="appearance.icons" label="Icons" value={draftValue<'nerd' | 'plain' | 'off'>('appearance.icons')}
          choices={icons} disabled={disabled} onChange={value => edit('appearance.icons', value)} />
        <ChoiceSetting field="appearance.static_marks" label="Activity marks" value={draftValue<boolean>('appearance.static_marks')}
          choices={marks} disabled={disabled} onChange={value => edit('appearance.static_marks', value)} />
        <ChoiceSetting field="appearance.text_git_marks" label="Git status marks" value={draftValue<boolean>('appearance.text_git_marks')}
          choices={gitMarks} disabled={disabled} onChange={value => edit('appearance.text_git_marks', value)} />
      </SettingSection>
    </GroupCard>
  );
}
