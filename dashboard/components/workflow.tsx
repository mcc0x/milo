'use client';

import { ChoiceSetting, GroupCard, SwitchSetting } from './controls';
import Presence from './presence';
import { useSession } from './session';

const tracking = [['follow', 'Follow shell'], ['pin', 'Pin project']] as const;
const outline = [[false, 'All files'], [true, 'Changes only']] as const;

export default function Workflow() {
  const { state, draftValue, edit } = useSession();
  const disabled = !state.groups.preferences.editable;

  return (
    <>
      <GroupCard group="preferences" title="Pane workflow">
        <ChoiceSetting field="workflow.project_mode" label="Project tracking"
          value={draftValue<'follow' | 'pin'>('workflow.project_mode')} choices={tracking}
          disabled={disabled} onChange={value => edit('workflow.project_mode', value)} />
        <SwitchSetting field="workflow.follow_activity" label="Follow file edits" detail="Temporarily reveal active files."
          checked={draftValue<boolean>('workflow.follow_activity')}
          disabled={disabled} onChange={checked => edit('workflow.follow_activity', checked)} />
        <ChoiceSetting field="workflow.changes_only" label="Outline starts with"
          value={draftValue<boolean>('workflow.changes_only')} choices={outline}
          disabled={disabled} onChange={value => edit('workflow.changes_only', value)} />
        <SwitchSetting field="workflow.history_follow_new" label="Follow new commits" detail="Only while History is at the top."
          checked={draftValue<boolean>('workflow.history_follow_new')}
          disabled={disabled} onChange={checked => edit('workflow.history_follow_new', checked)} />
        <SwitchSetting field="workflow.tab_titles" label="Title Rex tabs" detail="Unnamed tabs only: project, worktree and agent."
          checked={draftValue<boolean>('workflow.tab_titles')}
          disabled={disabled} onChange={checked => edit('workflow.tab_titles', checked)} />
      </GroupCard>
      <GroupCard group="account" title="Claude Code">
        <SwitchSetting field="account.claude_limits" label="Show Claude Code limits" detail={state.account.claude_status}
          checked={draftValue<boolean>('account.claude_limits')} disabled={!state.groups.account.editable}
          onChange={checked => edit('account.claude_limits', checked)} />
      </GroupCard>
      <Presence />
    </>
  );
}
