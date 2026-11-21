export default function IntegrationSwitch({id, label, enabled, disabled, onClick}: {
  id: string; label: string; enabled: boolean; disabled: boolean; onClick: () => void;
}) {
  return <button id={id} type="button" role="switch" aria-label={label} aria-checked={enabled}
    className="integration-switch" disabled={disabled} onClick={onClick}>
    <span>{enabled ? 'On' : 'Off'}</span><i aria-hidden="true" />
  </button>;
}
