export type GroupKey = 'layout' | 'preferences' | 'account';
export type Scope = 'worktree' | 'global' | 'account';
export type PaneRole = 'outline' | 'history' | 'usage';
export type UsageChoice = 'auto' | 'always' | 'hide';
export type PresenceDetails = 'generic' | 'session' | 'project' | 'both';
export type AlertKind = 'blocked' | 'error' | 'done' | 'notification' | 'bell';
export type AlertDestination = 'telegram' | 'discord';

export interface GroupState {
  scope: Scope | 'default';
  dirty: boolean;
  editable: boolean;
  save_global: boolean;
  save_worktree: boolean;
  save_account: boolean;
  inherit: boolean;
}

export interface PresenceState {
  enabled: boolean;
  details: PresenceDetails;
  application_id: string | null;
  official_id: string;
  status: string;
  help: string;
  public: Record<PresenceDetails, string>;
}

export interface AlertsState {
  enabled: boolean;
  running: boolean;
  destination: AlertDestination | null;
  consent: string | null;
  telegram: {saved: boolean; active: boolean; chat: string; consent: string | null; needs_local_forget: boolean};
  discord: {saved: boolean; active: boolean; consent: string | null};
  events: Record<AlertKind, boolean>;
  content: 'metadata' | 'text';
  place: string;
  disclosure: string;
  note: string;
}

/** The computer this editor runs on and the Rex that serves it. */
export interface HostState {
  name: string;
  /** The page is open on another computer, through a Rex forward. */
  forwarded: boolean;
  rex: {
    host_id: string;
    version: string;
    api: number;
    checked_api: number;
    command: {version: string; api: number} | null;
    compatibility: 'checked' | 'newer' | 'older' | 'command_differs';
    warning: boolean;
    note: string;
    viewers: 'here' | 'elsewhere' | 'nobody' | null;
  } | null;
}

export type TransferGroup = 'appearance' | 'workflow' | 'usage' | 'layout';
export type TransferScope = 'global' | 'worktree' | 'all';

/** What an import would change here, worked out by the editor before anything is written. */
export interface TransferPreview {
  basis: string;
  changes: number;
  sections: {
    kind: 'global' | 'worktree' | 'unmapped';
    project: string | null;
    path: string | null;
    why: string | null;
    unchanged: number;
    rows: {label: string; from: string; to: string}[];
    skipped: string[];
  }[];
}

export const healthLabels = {good: 'All good', warning: 'Warning', issue: 'Issue found'};

export interface SessionState {
  version: number;
  milo_version: string;
  context: {inside_rex: boolean; worktree: string | null; directory: string | null; installed: boolean};
  layout: {
    panes: Record<PaneRole, boolean>;
    side: 'left' | 'right';
    width: number;
    order: [PaneRole, PaneRole, PaneRole];
    usage_rows: number;
    ratios: [number, number, number] | null;
    auto_arrange: boolean;
  };
  appearance: {
    icons: 'nerd' | 'plain' | 'off';
    static_marks: boolean;
    text_git_marks: boolean;
    usage_large_figures: boolean;
    usage_codex: UsageChoice;
    usage_claude: UsageChoice;
    usage_opencode: UsageChoice;
    usage_deepseek: UsageChoice;
    usage_openrouter: UsageChoice;
    usage_devin: UsageChoice;
    usage_copilot: UsageChoice;
  };
  workflow: {
    project_mode: 'follow' | 'pin';
    follow_activity: boolean;
    changes_only: boolean;
    history_follow_new: boolean;
    tab_titles: boolean;
    preview_placement: 'right' | 'left' | 'above' | 'below' | 'floating';
  };
  account: {claude_limits: boolean; claude_status: string};
  menubar: {installed: boolean | null};
  groups: Record<GroupKey, GroupState>;
  capabilities: {capture: boolean; try: boolean; undo: boolean; auto_arrange: boolean};
  dirty: boolean;
  trial: boolean;
  doctor: {level: keyof typeof healthLabels; note: string} | null;
  message: {text: string; error: boolean} | null;
  reopen_panes: {version: number; text: string} | null;
  transfer: {undo: boolean};
  host: HostState | null;
  alerts: AlertsState | null;
  presence: PresenceState | null;
}

export const groupLabels: Record<GroupKey, string> = {
  layout: 'layout', preferences: 'pane preferences', account: 'Claude Code choices',
};

export function canSaveDraft(state: SessionState, key: GroupKey) {
  const group = state.groups[key];
  return (group.dirty || (key === 'layout' && state.trial)) && canWriteGroup(group);
}

export function canWriteGroup(group: GroupState) {
  return group.save_global || group.save_worktree || group.save_account;
}
