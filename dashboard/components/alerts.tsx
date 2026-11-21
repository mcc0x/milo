"use client";

import { useLayoutEffect, useRef, useState, type FormEvent, type ReactNode, type RefObject } from "react";
import { ChoiceSetting, SwitchSetting } from "./controls";
import HostedAlertsSetup from "./hosted-alerts-setup";
import { useSession, type TelegramChat } from "./session";
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
type Field = "forwarding" | "content" | "test" | "forget" | "forget_local" | Destination | `events.${(typeof events)[number][0]}`;

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
function DestinationInput({ id, label, disabled, secret = false, initial, onInput, children }: {
  id: string;
  label: string;
  disabled: boolean;
  secret?: boolean;
  initial?: string;
  onInput?: () => void;
  children?: ReactNode;
}) {
  return (
    <div className="alerts-field">
      <label className="setting-label" htmlFor={id}>{label}</label>
      <div className={children ? "alerts-entry" : undefined}>
      <input type="text" id={id} className={secret ? "masked" : undefined} placeholder={secret ? " " : undefined}
        autoComplete="off" autoCapitalize="off" spellCheck={false} required disabled={disabled} defaultValue={initial} onInput={onInput} />
        {children}
      </div>
    </div>
  );
}

function valueOf(form: HTMLFormElement, id: string) {
  return (form.elements.namedItem(id) as HTMLInputElement).value.trim();
}

function DestinationSave({ name, label, disabled }: { name: Destination; label: string; disabled: boolean }) {
  return (
    <button type="submit" id={`alerts-${name}-save`} aria-label={`Save ${label}`} className="button primary alerts-save" disabled={disabled}>
      <svg width="15" height="15" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="m4 10 4 4 8-8"/></svg>
      Save
    </button>
  );
}

type TelegramStep =
  | { kind: "token" | "loading" | "empty" | "error" | "manual" }
  | { kind: "choose"; chats: TelegramChat[] };
const telegramStart = { kind: "token" } as const;
const telegramStatus = {
  token: "", manual: "",
  loading: "Looking for recent chats…",
  empty: "No recent chats found. Message the bot and try again, or enter the chat manually.",
  error: "Couldn’t find chats. Try again or enter the chat manually. Bots with a webhook need manual entry.",
  choose: "Choose a chat to save.",
} satisfies Record<TelegramStep["kind"], string>;
const telegramTokenId = "alerts-telegram-token";
const telegramChatId = "alerts-telegram-chat";

function TelegramSetup({ form, busy, savedChat, save }: {
  form: RefObject<HTMLFormElement | null>;
  busy: boolean;
  savedChat: string;
  save: (value: { token: string; chat: string }) => void;
}) {
  const { findTelegramChats } = useSession();
  const [step, setStep] = useState<TelegramStep>(telegramStart);
  const request = useRef<AbortController | null>(null);
  useLayoutEffect(() => () => request.current?.abort(), []);
  useLayoutEffect(() => {
    if (step.kind === "manual") {
      (form.current?.elements.namedItem(telegramChatId) as HTMLInputElement | null)?.focus();
    }
  }, [form, step.kind]);

  const canSave = step.kind === "choose" || step.kind === "manual";

  function cancelLookup() {
    request.current?.abort();
    request.current = null;
  }

  function reset() {
    cancelLookup();
    setStep(telegramStart);
  }

  async function find() {
    if (busy || request.current || !form.current) return;
    const input = form.current.elements.namedItem(telegramTokenId) as HTMLInputElement;
    input.value = input.value.trim();
    if (!input.reportValidity()) return;
    const lookup = new AbortController();
    request.current = lookup;
    setStep({ kind: "loading" });
    try {
      const { chats } = await findTelegramChats(input.value, lookup.signal);
      if (!lookup.signal.aborted) setStep(chats.length ? { kind: "choose", chats } : { kind: "empty" });
    } catch {
      if (!lookup.signal.aborted) setStep({ kind: "error" });
    } finally {
      if (request.current === lookup) request.current = null;
    }
  }

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy) return;
    if (!canSave) {
      void find();
      return;
    }
    const current = event.currentTarget;
    save({ token: valueOf(current, telegramTokenId), chat: valueOf(current, telegramChatId) });
  }

  return (
    <form id="alerts-telegram" className="secret-form alerts-form alerts-telegram-form" autoComplete="off" ref={form}
      onSubmit={submit} onReset={reset}>
      <p className="setting-detail">Get a token from <strong>@BotFather</strong>. Message the bot, send it a command in your group, or add it as a channel admin and post there.</p>
      <DestinationInput id={telegramTokenId} label="Bot token" disabled={busy} secret
        onInput={() => { if (step.kind !== "manual") reset(); }}>
        <button type="button" className={`button ${canSave ? "secondary" : "primary"}`} disabled={busy}
          aria-disabled={step.kind === "loading" || undefined} onClick={find}>Find chats</button>
      </DestinationInput>
      <p className="setting-detail alerts-telegram-status" role="status">{telegramStatus[step.kind]}</p>
      {step.kind === "choose" ? (
        <div className="alerts-field">
          <label className="setting-label" htmlFor={telegramChatId}>Telegram chat</label>
          <select id={telegramChatId} defaultValue="" required disabled={busy}>
            <option value="" disabled>Choose a chat</option>
            {step.chats.map(({ id, title }) => <option key={id} value={id}>{title} · {id}</option>)}
          </select>
        </div>
      ) : step.kind === "manual" ? (
        <DestinationInput id={telegramChatId} label="Chat ID or @channel" disabled={busy} initial={savedChat} />
      ) : null}
      <div className="button-row alerts-telegram-actions">
        {step.kind !== "manual" ? (
          <button type="button" className="button text-button" disabled={busy}
            onClick={() => { cancelLookup(); setStep({ kind: "manual" }); }}>Enter chat manually</button>
        ) : null}
        {canSave ? <DestinationSave name="telegram" label="Telegram" disabled={busy} /> : null}
      </div>
    </form>
  );
}

export default function Alerts() {
  const { state, paused, running, closing, command, confirm, localError, operationWarning, toast } = useSession();
  const form = useRef<HTMLFormElement>(null);
  const manager = useRef<HTMLDialogElement>(null);
  const [editor, setEditor] = useState<DestinationEditor>("closed");
  const previousPage = useRef<DestinationEditor>("closed");
  useLayoutEffect(() => {
    const previous = previousPage.current;
    previousPage.current = editor;
    if (editor === "discord" || editor === "telegram") manager.current?.querySelector<HTMLElement>("h2")?.focus();
    else if (editor === "list" && (previous === "discord" || previous === "telegram"))
      manager.current?.querySelector<HTMLElement>(`#alerts-edit-${previous}`)?.focus();
  }, [editor]);
  const alerts = state.alerts;
  if (!alerts) return null;

  const busy = paused || running || closing;
  const active = destinations.filter(([name]) => alerts[name].active);
  const saved = destinations.filter(([name]) => alerts[name].saved);
  const targetLabels = { telegram: `Telegram chat ${alerts.telegram.chat}`, discord: "Discord channel" };
  const targets = (active.length ? active : saved).map(([name]) => targetLabels[name]);
  const status = !active.length ? "Forwarding off" : alerts.running ? "Forwarding on" : "Enabled, not running";
  const destinationsOpen = editor !== "closed";
  const actionError = localError || operationWarning;
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
    // The session keeps a credential out of its public queue and response.
    form.current?.reset();
    command("alerts", { field, value });
  }

  function forgetTelegramLocally() {
    confirm("Forget Telegram on this Mac?",
      "This removes the saved Telegram connection and stops its forwarding on this Mac. Discord is not changed. " +
      "It does not revoke the connection on the relay. If the original relay is still available, send /stop to Milo’s bot " +
      "in the original chat to revoke all Milo connections to that chat.",
      "Forget on this Mac", () => {
        manager.current?.querySelector<HTMLElement>("h2")?.focus();
        send("forget_local", "telegram");
      });
  }

  function saveDiscord(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!busy) send("discord", valueOf(event.currentTarget, "alerts-discord-link"));
  }

  function toggleForwarding(destination: Destination) {
    const consent = alerts?.[destination].consent;
    if (alerts?.[destination].active) {
      send("forwarding", { destination, enabled: false });
    } else if (consent) {
      const selected = selectedEvents.length ? `Events: ${selectedEvents.map(([, label]) => label).join(", ")}.` : noEvents;
      confirm("Turn on alert forwarding?",
        `Destination: ${targetLabels[destination]}.\n${selected}\n${payload}\n${rexNotifications}`, "Turn on",
        () => send("forwarding", { destination, enabled: true, consent }));
    }
  }

  return (
    <div className="alert-settings">
      <AlertGroup id="group-alerts" title="Destinations">
        <div className="alerts-destination">
          <div className="alerts-destination-copy">
            <div className="alerts-destination-line">
              <p className="setting-label">{targets.length ? targets.join(" and ") : "No destination"}</p>
              <span className="setting-detail alerts-status" id="group-alerts-status" role="status"
                data-running={active.length > 0 && alerts.running}>{status}</span>
            </div>
            {!targets.length ? <p className="setting-detail">Connect a chat or channel. Forwarding stays off until you turn it on.</p> : null}
            {alerts.note ? <p className="setting-detail disclosure-text">{alerts.note}</p> : null}
          </div>
          <div className="button-row">
            <button type="button" className={`button ${targets.length ? "secondary" : "primary"}`}
              id="alerts-destinations-toggle" aria-haspopup="dialog" aria-controls="alerts-destinations"
              onClick={() => { showEditor("list"); manager.current?.showModal(); }}>
              {targets.length ? "Manage destinations" : "Set up destination"}
            </button>
          </div>
        </div>
        <dialog ref={manager} id="alerts-destinations" className="alerts-manager" aria-labelledby="alerts-manager-title"
          onClose={() => showEditor("closed")}>
          <header className="alerts-manager-heading">
            <button type="button" className="integration-back" disabled={editor === "list" || !destinationsOpen}
              aria-label="Back to destinations" onClick={() => showEditor("list")}>
              <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M19 12H5m6-6-6 6 6 6"/></svg>
            </button>
            <h2 id="alerts-manager-title" tabIndex={-1}>Destinations{destinations.map(([key, label]) =>
              editor === key && <span key={key}> / {label}</span>)}</h2>
            <button type="button" className="button text-button" aria-label="Close destinations" onClick={() => manager.current?.close()}>Done</button>
          </header>
          {destinationsOpen ? (
            <div className="alerts-manager-body" key={editor}>
              {editor === "list" && <div className="alerts-provider-picker" role="group" aria-label="Choose a destination">
                {destinations.map(([name, label]) => {
                  const { active, saved, consent } = alerts[name];
                  return (
                    <div className="alerts-provider-row" key={name}>
                    <button type="button" className="alerts-provider" id={`alerts-edit-${name}`} aria-label={label}
                      onClick={() => showEditor(name)}>
                      <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                        {name === "telegram" ? <path d="M21.5 3.7 18 20.1c-.2.9-.8 1.1-1.5.6l-5.3-3.9-2.5 2.4.4-4.8L18 6.6c.4-.4.1-.6-.4-.3L6.5 13.2 2 11.8c-1-.3-1-1 .2-1.5L20.5 3.3c.7-.3 1.2-.1 1 .4Z"/> :
                          <path fillRule="evenodd" d="M8.5 4.5c-1.6.2-3 .7-4.1 1.3C2 9.4 1.1 13.3 1.5 17c1.6 1.2 3.2 2 5.1 2.5l1.1-1.7-1.7-.9.4-.3c3.5 1.6 7.7 1.6 11.2 0l.4.3-1.7.9 1.1 1.7c1.9-.5 3.5-1.3 5.1-2.5.4-3.7-.5-7.6-2.9-11.2-1.1-.6-2.5-1.1-4.1-1.3l-.6 1.2a15 15 0 0 0-5.8 0ZM8.4 14.6a1.6 2 0 1 1 0-4 1.6 2 0 1 1 0 4Zm7.2 0a1.6 2 0 1 1 0-4 1.6 2 0 1 1 0 4Z"/>}
                      </svg>
                      <span>{label}<small>{active ? (alerts.running ? "Forwarding on" : "Enabled, not running") : saved ? "Forwarding off" : "Not connected"}</small></span>
                      <span className="alerts-edit-label">{saved ? "Edit" : "Set up"}</span>
                    </button>
                    {active || saved ? (
                      <div className="alerts-provider-controls">
                        <IntegrationSwitch id={`alerts-forward-${name}`} label={`Forward to ${label}`} enabled={active} disabled={busy || (!active && !consent)}
                          onClick={() => toggleForwarding(name)} />
                        {saved ? <button type="button" className="button text-button" id={`alerts-test-${name}`} disabled={busy}
                          aria-label={`Send test to ${label}`} aria-describedby="alerts-test-detail"
                          onClick={() => send("test", name)}>Send test</button> : null}
                      </div>
                    ) : null}
                    </div>
                  );
                })}
                {saved.length ? <p className="setting-detail" id="alerts-test-detail">A test sends one message from Milo, not a Rex alert.</p> : null}
              </div>}
              <div id="alerts-provider-setup">
                {destinations.map(([name, label]) => editor === name && (
                  <div key={name}>
                    {alerts[name].saved ? <p className="setting-detail alerts-saved-destination">Saved destination{name === "telegram" ? ` · ${alerts.telegram.chat}` : ""}</p> : null}
                    {name === "telegram" && alerts.telegram.needs_local_forget ? (
                      <div className="alerts-setup">
                        <p className="setting-detail">Milo can’t disconnect this Telegram connection using the current relay. Return to the original relay if possible, or forget the connection on this Mac before connecting again.</p>
                        <button type="button" className="button alerts-forget" id="alerts-forget-telegram-local"
                          disabled={busy} onClick={forgetTelegramLocally}>Forget on this Mac</button>
                      </div>
                    ) : <HostedAlertsSetup provider={name} label={label} busy={busy}
                      replacingActive={alerts[name].active}
                      forget={alerts[name].saved ? () => confirm(`Forget ${label}?`,
                        `This removes the saved ${label} destination and its secret from ${alerts.place}. Forwarding to it ends.`,
                        "Forget", () => send("forget", name)) : undefined}>
                      {name === "telegram" ? (
                        <TelegramSetup form={form} busy={busy} savedChat={alerts.telegram.chat} save={value => {
                          manager.current?.querySelector<HTMLElement>("h2")?.focus();
                          send("telegram", value);
                        }} />
                      ) : (
                        <form id="alerts-discord" className="secret-form alerts-form" autoComplete="off" ref={form}
                          onSubmit={saveDiscord}>
                          <ol className="setting-detail alerts-steps" aria-label="Discord setup steps">
                            <li>In Discord, open <strong>Edit Channel</strong> for the channel you want Milo to post in.</li>
                            <li>Go to <strong>Integrations → Webhooks</strong>. Choose a webhook or create one.</li>
                            <li>Select <strong>Copy Webhook URL</strong>, paste it below, then choose <strong>Save</strong>.</li>
                          </ol>
                          <DestinationInput id="alerts-discord-link" label="Webhook link" disabled={busy} secret>
                            <DestinationSave name="discord" label="Discord" disabled={busy} />
                          </DestinationInput>
                        </form>
                      )}
                    </HostedAlertsSetup>}
                  </div>
                ))}
              </div>
              <p role="status" className="setting-detail alerts-action-note" data-error={!!actionError}>{actionError || toast?.text || alerts.note}</p>
            </div>
          ) : null}
          <footer className="setting-detail alerts-storage"><strong>Credentials:</strong> stored in {alerts.place}. Saved secrets aren’t shown again.</footer>
        </dialog>
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
