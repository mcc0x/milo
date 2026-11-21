"use client";

import { useRef, useState, type FormEvent, type ReactNode } from "react";
import { ChoiceSetting, SettingRow, SwitchSetting } from "./controls";
import { useSession } from "./session";
import IntegrationSwitch from "./integration-switch";
import "./alerts.css";

const destinations = [["telegram", "Telegram"], ["discord", "Discord"]] as const;
const events = [
  ["blocked", "Waiting for you", "A program is blocked on a permission, a question or a sign-in."],
  ["error", "Reported an error", "A program says it failed."],
  ["done", "Finished", "A program says it finished."],
  ["notification", "Desktop notification", "A program asks for one. A repeat within 30 seconds is not sent."],
  ["bell", "Terminal bell", "At most one per terminal in 30 seconds."],
] as const;
const content = [["metadata", "Kind and program name"], ["text", "Also program text"]] as const;

type Destination = (typeof destinations)[number][0];
type DestinationEditor = "closed" | "list" | Destination;
type Field = "enabled" | "content" | "destination" | "test" | "forget" | Destination | `events.${(typeof events)[number][0]}`;

function AlertGroup({ id, title, children }: { id: string; title: string; children: ReactNode }) {
  return (
    <section className="settings-group" id={id} aria-labelledby={`${id}-title`}>
      <h3 className="alerts-heading" id={`${id}-title`}>{title}</h3>
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
  return (
    <label className="alerts-field">
      <span className="setting-label">{label}</span>
      <input type="text" id={id} className={secret ? "masked" : undefined} placeholder={secret ? " " : undefined}
        autoComplete="off" autoCapitalize="off" spellCheck={false} required disabled={disabled} defaultValue={initial} />
    </label>
  );
}

function valueOf(form: HTMLFormElement, id: string) {
  return (form.elements.namedItem(id) as HTMLInputElement).value.trim();
}

export default function Alerts() {
  const { state, paused, running, closing, command, confirm } = useSession();
  const form = useRef<HTMLFormElement>(null);
  const [editor, setEditor] = useState<DestinationEditor>("closed");
  const alerts = state.alerts;
  if (!alerts) return null;

  const busy = paused || running || closing;
  const { destination, enabled } = alerts;
  const target = destination === "telegram" ? `Telegram chat ${alerts.telegram.chat}` :
    destination === "discord" ? "Discord channel of the saved webhook" : null;
  const status = !enabled ? "Off" : alerts.running ? "On" : "On, but not running";
  const destinationsOpen = editor !== "closed";
  const selectedEvents = events.filter(([kind]) => alerts.events[kind]);
  const payload = alerts.content === "text"
    ? "Each alert includes its kind, the program’s short name, and up to 300 characters of program text. That text can name files or quote a prompt. No session, tab or folder names are added."
    : "Each alert includes only its kind and the program’s short name. No text from the program is sent. No session, tab, folder or file names are included.";
  const rexNotifications = "Rex’s own notifications are unchanged.";
  const noEvents = "No events selected. Nothing is forwarded until you select an event.";

  function showEditor(next: DestinationEditor) {
    form.current?.reset();
    setEditor(next);
  }

  function send(field: Field, value: unknown) {
    if (busy) return;
    // Clear unsent inputs when making a change, before the request can settle.
    // Secrets never enter React state, and the response never repopulates them.
    form.current?.reset();
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

  function toggleForwarding() {
    if (enabled) {
      send("enabled", false);
    } else if (target) {
      const selected = selectedEvents.length ? `Events: ${selectedEvents.map(([, label]) => label).join(", ")}.` : noEvents;
      confirm("Turn on alert forwarding?",
        `Destination: ${target}.\n${selected}\n${payload}\n${rexNotifications}`, "Turn on", () => send("enabled", true));
    }
  }

  return (
    <div className="alert-settings">
      <AlertGroup id="group-alerts" title="Destination">
        <div className="alerts-destination">
          <div className="alerts-destination-copy">
          <div className="alerts-destination-line">
            <p className="setting-label">{target ?? "No destination"}</p>
            <span className="setting-detail alerts-status" id="group-alerts-status" role="status"
              data-running={enabled && alerts.running}>{status}</span>
          </div>
          {!target ? <p className="setting-detail">Choose a Telegram chat or Discord channel. Saving won’t turn forwarding on.</p> : null}
          {alerts.note ? <p className="setting-detail disclosure-text">{alerts.note}</p> : null}
          </div>
          <div className="button-row">
            {(enabled || target) ? (
              <IntegrationSwitch id="alerts-toggle" label="Alert forwarding" enabled={enabled} disabled={busy} onClick={toggleForwarding}/>
            ) : null}
            <button type="button" className={`button ${target || destinationsOpen ? "secondary" : "primary"}`}
              id="alerts-destinations-toggle" aria-expanded={destinationsOpen} aria-controls="alerts-destinations"
              onClick={() => showEditor(destinationsOpen ? "closed" : "list")}>
              {destinationsOpen ? "Close destinations" : target ? "Change destination" : "Set up destination"}
            </button>
            {target ? (
              <button type="button" className="button text-button" id="alerts-test" disabled={busy}
                aria-describedby="alerts-test-detail" onClick={() => send("test", null)}>Send test</button>
            ) : null}
          </div>
          {target ? (
            <p className="setting-detail" id="alerts-test-detail">A test sends one message from Milo, not a Rex alert.</p>
          ) : null}
        </div>
        <div id="alerts-destinations" className="alerts-destinations" hidden={!destinationsOpen}>
          {destinationsOpen ? (
            <>
              <h4 className="setting-label">Destinations</h4>
              <p className="setting-detail">Milo stores credentials in {alerts.place}. Once saved, they aren’t shown again.</p>
              <div className="group-controls">
                {destinations.map(([name, label]) => {
                  const saved = alerts[name].saved;
                  const selected = destination === name;
                  const open = editor === name;
                  const chat = saved && name === "telegram" ? `: ${alerts.telegram.chat}` : "";
                  return (
                    <div key={name}>
                      <SettingRow label={label} detail={`${selected ? "Selected" : saved ? "Saved" : "Not saved"}${chat}`}>
                        <div className="button-row">
                          {saved && !selected ? (
                            <button type="button" className="button secondary" id={`alerts-use-${name}`} disabled={busy}
                              onClick={() => send("destination", name)}>Use {label}</button>
                          ) : null}
                          <button type="button" className="button secondary" id={`alerts-edit-${name}`}
                            aria-expanded={open} aria-controls={`alerts-${name}-setup`}
                            onClick={() => showEditor(open ? "list" : name)}>
                            {open ? `Close ${label} setup` : `${saved ? "Edit" : "Add"} ${label}`}
                          </button>
                          {saved ? (
                            <button type="button" className="button text-button" id={`alerts-forget-${name}`} disabled={busy}
                              aria-label={`Forget ${label}…`} onClick={() => confirm(`Forget ${label}?`,
                                `This removes the saved ${label} destination and its secret from ${alerts.place}. Forwarding to it ends.`,
                                "Forget", () => send("forget", name))}>Forget…</button>
                          ) : null}
                        </div>
                      </SettingRow>
                      <div id={`alerts-${name}-setup`} hidden={!open}>
                        {open ? (
                          <form id={`alerts-${name}`} className="secret-form alerts-form" autoComplete="off" ref={form}
                            onSubmit={name === "telegram" ? saveTelegram : saveDiscord}>
                            <p className="setting-detail">{name === "telegram"
                              ? "Enter the chat number or a public channel’s @name, and the bot token from BotFather."
                              : "Copy the webhook link from the channel’s Integrations settings."}</p>
                            {name === "telegram" ? (
                              <>
                                <DestinationInput key={alerts.telegram.chat} id="alerts-telegram-chat" label="Chat"
                                  disabled={busy} initial={alerts.telegram.chat} />
                                <DestinationInput id="alerts-telegram-token" label="Bot token" disabled={busy} secret />
                              </>
                            ) : <DestinationInput id="alerts-discord-link" label="Webhook link" disabled={busy} secret />}
                            <button type="submit" id={`alerts-${name}-save`} className="button secondary" disabled={busy}>
                              Save {label}
                            </button>
                          </form>
                        ) : null}
                      </div>
                    </div>
                  );
                })}
              </div>
            </>
          ) : null}
        </div>
      </AlertGroup>
      <AlertGroup id="group-alert-content" title="What gets sent">
        <p className="setting-detail alerts-privacy">{payload} {rexNotifications}</p>
        <ChoiceSetting field="alerts.content" label="Content"
          value={alerts.content} choices={content} disabled={busy} onChange={key => send("content", key)} />
      </AlertGroup>
      <AlertGroup id="group-alert-kinds" title="Events to forward">
        {!selectedEvents.length ? <p className="setting-detail alerts-privacy">{noEvents}</p> : null}
        {events.map(([kind, label, detail]) => (
          <SwitchSetting key={kind} field={`alerts.events.${kind}`} label={label} detail={detail}
            checked={alerts.events[kind]} disabled={busy} onChange={checked => send(`events.${kind}`, checked)} />
        ))}
      </AlertGroup>
    </div>
  );
}
