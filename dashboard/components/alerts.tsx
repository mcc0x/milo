"use client";

import { useRef, type FormEvent, type ReactNode } from "react";
import { ChoiceSetting, SettingRow, SwitchSetting } from "./controls";
import { useSession } from "./session";

const destinations = { telegram: "Telegram", discord: "Discord" } as const;
const events = [
  ["blocked", "Waiting for you", "A program is blocked on a permission, a question or a sign-in."],
  ["error", "Reported an error", "A program says it failed."],
  ["done", "Finished", "A program says it finished."],
  ["notification", "Desktop notification", "A program asks for one. A repeat within 30 seconds is not sent."],
  ["bell", "Terminal bell", "At most one per terminal in 30 seconds."],
] as const;
const content = [["metadata", "Kind and program name"], ["text", "Also program text"]] as const;

type Destination = keyof typeof destinations;
type Field = "enabled" | "content" | "destination" | "test" | "forget" | Destination | `events.${(typeof events)[number][0]}`;

function AlertGroup({ id, title, status, children }: { id: string; title: string; status?: string; children: ReactNode }) {
  return (
    <section className="settings-group" aria-labelledby={id}>
      <header className="group-header group-titleline">
        <h2 id={id}>{title}</h2>
        {status ? <span className="setting-detail" id={`${id}-status`} role="status">{status}</span> : null}
      </header>
      <div className="group-controls">{children}</div>
    </section>
  );
}

// A masked text control avoids password-manager prompts. No name or default
// value for a secret: it can leave the form only through the explicit POST.
function DestinationInput({ id, label, disabled, secret = false, initial }: {
  id: string;
  label: string;
  disabled: boolean;
  secret?: boolean;
  initial?: string;
}) {
  return <input type="text" id={id} className={secret ? "masked" : undefined} placeholder={label} aria-label={label}
    autoComplete="off" autoCapitalize="off" spellCheck={false} required disabled={disabled} defaultValue={initial} />;
}

function valueOf(form: HTMLFormElement, id: string) {
  return (form.elements.namedItem(id) as HTMLInputElement).value.trim();
}

export default function Alerts() {
  const { state, paused, running, closing, command, confirm } = useSession();
  const telegram = useRef<HTMLFormElement>(null);
  const discord = useRef<HTMLFormElement>(null);
  const alerts = state.alerts;
  if (!alerts) return null;

  const busy = paused || running || closing;
  const target = alerts.destination === "telegram" ? `Telegram chat ${alerts.telegram.chat}` :
    alerts.destination === "discord" ? "the Discord channel of the saved webhook" : "";
  const status = !alerts.enabled ? "Off" : alerts.running ? "On" : "On, but not running";

  function send(field: Field, value: unknown) {
    if (busy) return;
    // Clear unsent inputs when making a change, before the request can settle.
    // Secrets never enter React state, and the response never repopulates them.
    telegram.current?.reset();
    discord.current?.reset();
    command("alerts", { field, value });
  }

  function saveTelegram(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy) return;
    const form = event.currentTarget;
    send("telegram", { token: valueOf(form, "alerts-telegram-token"), chat: valueOf(form, "alerts-telegram-chat") });
  }

  function saveDiscord(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!busy) send("discord", valueOf(event.currentTarget, "alerts-discord-link"));
  }

  const destinationRow = (name: Destination) => {
    const selected = alerts.destination === name;
    const saved = alerts[name].saved;
    const label = destinations[name];
    const detail = !saved ? `Not saved. Once saved, its secret is kept in ${alerts.place} and is never shown.` :
      `${selected ? "Saved and selected." : "Saved."} The secret is kept in ${alerts.place} and is never shown again.`;
    return (
      <SettingRow label={label} detail={detail}>
        <div className="button-row">
          {saved && !selected ? (
            <button type="button" className="button secondary" id={`alerts-use-${name}`} disabled={busy}
              onClick={() => send("destination", name)}>
              Use {label}
            </button>
          ) : null}
          {saved ? (
            <button type="button" className="button text-button" id={`alerts-forget-${name}`} disabled={busy}
              onClick={() => confirm(`Forget ${label}?`,
                `This removes the saved ${label} destination and its secret from ${alerts.place}. Mirroring to it ends.`,
                "Forget", () => send("forget", name))}>
              Forget…
            </button>
          ) : null}
        </div>
      </SettingRow>
    );
  };

  return (
    <>
      <AlertGroup id="group-alerts" title="Mirror Rex alerts" status={status}>
        <SettingRow label="Send Rex alerts to a chat or channel you choose"
          detail={`${alerts.enabled ? "On." : "Off until you turn it on."} Rex’s own notifications stay as they are. ` +
            (target ? `Alerts go to ${target}.` : "Save a destination below first.")}>
          <button type="button" className="button secondary" id="alerts-toggle"
            disabled={busy || (!alerts.enabled && !alerts.destination)} onClick={() => {
              if (alerts.enabled) send("enabled", false);
              else if (alerts.destination) confirm("Mirror Rex alerts?",
                `Alerts go to ${target}.\n${alerts.disclosure}\nRex’s own notifications are unchanged.`,
                "Turn on", () => send("enabled", true));
            }}>
            {alerts.enabled ? "Turn off" : "Turn on…"}
          </button>
        </SettingRow>
        <SettingRow label="What is sent" detail={alerts.disclosure + (alerts.note ? `\n${alerts.note}` : "")}
          detailClassName="setting-detail disclosure-text" />
        <ChoiceSetting field="alerts.content" label="Content" detail="Program text can name files or quote a prompt."
          value={alerts.content} choices={content} disabled={busy} onChange={key => send("content", key)} />
        <SettingRow label="Test message" detail="One message written by Milo. No Rex alert is involved.">
          <button type="button" className="button secondary" id="alerts-test" disabled={busy || !alerts.destination}
            onClick={() => { if (alerts.destination) send("test", null); }}>
            Send test
          </button>
        </SettingRow>
      </AlertGroup>
      <AlertGroup id="group-alert-kinds" title="Alerts to mirror">
        {events.map(([kind, label, detail]) => (
          <SwitchSetting key={kind} field={`alerts.events.${kind}`} label={label} detail={detail}
            checked={alerts.events[kind]} disabled={busy} onChange={checked => send(`events.${kind}`, checked)} />
        ))}
      </AlertGroup>
      <AlertGroup id="group-alert-destinations" title="Destinations">
        {destinationRow("telegram")}
        <SettingRow label="Telegram bot" detail="The chat’s number or a public channel’s @name, and the bot token from BotFather.">
          <form id="alerts-telegram" className="secret-form" autoComplete="off" ref={telegram} onSubmit={saveTelegram}>
            <DestinationInput key={alerts.telegram.chat} id="alerts-telegram-chat" label="Chat" disabled={busy}
              initial={alerts.telegram.chat} />
            <DestinationInput id="alerts-telegram-token" label="Bot token" disabled={busy} secret />
            <button type="submit" id="alerts-telegram-save" className="button secondary" disabled={busy}>Save Telegram</button>
          </form>
        </SettingRow>
        {destinationRow("discord")}
        <SettingRow label="Discord webhook" detail="The webhook link from the channel’s Integrations settings.">
          <form id="alerts-discord" className="secret-form" autoComplete="off" ref={discord} onSubmit={saveDiscord}>
            <DestinationInput id="alerts-discord-link" label="Webhook link" disabled={busy} secret />
            <button type="submit" id="alerts-discord-save" className="button secondary" disabled={busy}>Save Discord</button>
          </form>
        </SettingRow>
      </AlertGroup>
    </>
  );
}
