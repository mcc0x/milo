"use client";

import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { useSession, type AlertDestination } from "./session";

const providerSetup = {
  telegram: {
    description: "Send alerts to your private chat with Milo’s Telegram bot.",
    instructions: "Press Start in Milo’s Telegram chat and confirm the pairing.",
    manual: "Use your own bot",
  },
  discord: {
    description: "Let Milo post alerts to a Discord channel you choose.",
    instructions: "Choose a channel and authorize Milo in Discord.",
    manual: "Use your own webhook",
  },
} satisfies Record<AlertDestination, {description: string; instructions: string; manual: string}>;

type RelayMode = "production" | "local";
const keyPattern = /^[A-Za-z0-9_-]{43}$/;
const finishPattern = /^[0-9A-F]{8}$/;
const telegramLink = /^https:\/\/t\.me\/[A-Za-z0-9_]{5,32}\?start=([A-Za-z0-9_-]{43})$/;
const productionDiscordLink = /^https:\/\/connect\.usemilo\.net\/discord\/start\/([A-Za-z0-9_-]{43})$/;
const localDiscordLink = /^http:\/\/(?:127\.0\.0\.1|localhost):6120\/discord\/start\/([A-Za-z0-9_-]{43})$/;
const telegramMaxId = "9223372036854775807";
const discordMaxId = "18446744073709551615";

function exact(pattern: RegExp, value: string) {
  const match = pattern.exec(value);
  return match?.[0] === value ? match : undefined;
}

function idWithin(value: string, max: string) {
  return !!exact(/^[1-9][0-9]*$/, value) &&
    (value.length < max.length || (value.length === max.length && value <= max));
}

function fields(data: unknown, names: string[]): data is Record<string, unknown> {
  return data !== null && typeof data === "object" && !Array.isArray(data) &&
    Object.keys(data).length === names.length && names.every(name => Object.hasOwn(data, name));
}

function pairing(data: unknown, provider: AlertDestination): { deviceKey: string; url: string; mode: RelayMode } | undefined {
  if (!fields(data, ["deviceKey", "url", "mode"])) return;
  const { deviceKey, url, mode } = data;
  if (typeof deviceKey !== "string" || !exact(keyPattern, deviceKey) || typeof url !== "string" ||
    (mode !== "production" && mode !== "local")) return;
  const pattern = provider === "telegram" ? telegramLink : mode === "production" ? productionDiscordLink : localDiscordLink;
  const link = exact(pattern, url);
  if (link && link[1] !== deviceKey) return { deviceKey, url, mode };
}

function reviewLabel(data: unknown, provider: AlertDestination) {
  if (!fields(data, ["provider", "chat", "label"]) || data.provider !== provider) return;
  const { chat, label } = data;
  if (typeof chat !== "string" || typeof label !== "string") return;
  const channelLabel = "Discord channel · ";
  const valid = provider === "telegram" ? idWithin(chat, telegramMaxId) && label === `Telegram DM · ${chat}` :
    chat === "" && label.startsWith(channelLabel) && idWithin(label.slice(channelLabel.length), discordMaxId);
  if (valid) return label;
}

type Step =
  | { kind: "choose"; note?: string }
  | { kind: "starting" }
  | { kind: "waiting"; mode: RelayMode; note?: string; checking?: boolean; openManually: boolean }
  | { kind: "review"; label: string; note?: string; saving?: boolean }
  | { kind: "manual" };

type Connection = {
  request?: AbortController;
  pair?: { deviceKey: string; url: string };
  proposal?: { deviceKey: string; finishCode?: string };
};

function clearConnection(connection: Connection) {
  connection.request?.abort();
  connection.request = undefined;
  connection.pair = undefined;
  connection.proposal = undefined;
}

export default function HostedAlertsSetup({ provider, label, busy, replacingActive, forget, children }: {
  provider: AlertDestination;
  label: string;
  busy: boolean;
  replacingActive: boolean;
  forget?: () => void;
  children: ReactNode;
}) {
  const { hostedConnect, saveHostedConnection } = useSession();
  const [step, setStep] = useState<Step>({ kind: "choose" });
  // Keep the device key, launch URL and credentials out of render state and markup.
  const connection = useRef<Connection>({});
  const panel = useRef<HTMLDivElement>(null);
  const finishField = useRef<HTMLInputElement>(null);
  const previousStep = useRef(step.kind);
  const setup = providerSetup[provider];
  const replacementNotice = `Saving this replacement turns off forwarding to ${label}. Turn it back on when you’re ready.`;

  useLayoutEffect(() => {
    const current = connection.current;
    return () => clearConnection(current);
  }, []);
  useLayoutEffect(() => {
    if (previousStep.current === step.kind) return;
    previousStep.current = step.kind;
    panel.current?.querySelector<HTMLElement>(step.kind === "manual" ? "input" : "[data-setup-focus]")?.focus();
  }, [step.kind]);

  function cancel() {
    clearConnection(connection.current);
    setStep({ kind: "choose" });
  }

  async function start() {
    const current = connection.current;
    if (busy || current.request) return;
    const request = new AbortController();
    current.request = request;
    setStep({ kind: "starting" });
    // Reserve the new tab in the click handler so popup blockers don't block the pairing.
    let popup = window.open("about:blank", "_blank");
    if (popup) popup.opener = null;
    request.signal.addEventListener("abort", () => { popup?.close(); popup = null; }, { once: true });
    try {
      const { status, data } = await hostedConnect({ action: "start", provider }, request.signal);
      if (request.signal.aborted) return;
      const pair = status === 200 && pairing(data, provider);
      if (!pair) throw new Error();
      const { deviceKey, url, mode } = pair;
      current.pair = { deviceKey, url };
      const opened = popup && !popup.closed;
      if (opened) {
        popup?.location.replace(url);
        popup = null;
      }
      setStep({ kind: "waiting", mode, openManually: !opened, note: opened ? undefined : `Use Open ${label} to continue.` });
    } catch {
      if (!request.signal.aborted) {
        current.pair = undefined;
        setStep({ kind: "choose", note: "Connect with Milo is unavailable. Try again or use your own credentials." });
      }
    } finally {
      popup?.close();
      popup = null;
      if (current.request === request) current.request = undefined;
    }
  }

  function openPairingPage() {
    const pair = connection.current.pair;
    if (!pair || step.kind !== "waiting" || step.checking) return;
    const popup = window.open("about:blank", "_blank");
    if (!popup) return;
    popup.opener = null;
    popup.location.replace(pair.url);
    setStep({ ...step, openManually: false, note: undefined });
    panel.current?.querySelector<HTMLElement>("[data-setup-focus]")?.focus();
  }

  async function check(replacement?: string) {
    const current = connection.current;
    if (busy || step.kind !== "waiting" || !current.pair || current.request) return;
    const { deviceKey } = current.pair;
    const field = finishField.current;
    let finishCode: string | undefined;
    if (step.mode === "production") {
      if (!field) return;
      finishCode = (replacement ?? field.value).trim().toUpperCase();
      if (!exact(finishPattern, finishCode)) {
        if (replacement !== undefined) field.value = "";
        setStep({ ...step, note: "Enter the eight-character finish code to check the connection." });
        field.focus();
        return;
      }
      field.value = finishCode;
    }
    const request = new AbortController();
    current.request = request;
    setStep({ ...step, note: "Checking the connection…", checking: true });
    try {
      const { status, data } = await hostedConnect({ action: "preview", provider, deviceKey, finishCode }, request.signal);
      if (request.signal.aborted) return;
      if (status === 202 && fields(data, ["pending"]) && data.pending === true) {
        setStep({ ...step, note: `Not connected yet. Finish in ${label} and check again.` });
      } else if (status === 400 && step.mode === "production") {
        setStep({ ...step, note: "The finish code did not match. Check the code and try again." });
        finishField.current?.select();
      } else if (status === 404) {
        current.pair = undefined;
        setStep({ kind: "choose", note: "This connection expired or was already used. Start a new one or use your own credentials." });
      } else {
        const approved = status === 200 && reviewLabel(data, provider);
        if (!approved) throw new Error();
        current.proposal = { deviceKey, finishCode };
        current.pair = undefined;
        setStep({ kind: "review", label: approved });
      }
    } catch {
      if (!request.signal.aborted) setStep({ ...step, note: "Couldn’t check the connection. Try again, or cancel and use your own credentials." });
    } finally {
      if (current.request === request) current.request = undefined;
    }
  }

  async function saveProposal() {
    const current = connection.current;
    const proposal = current.proposal;
    if (busy || step.kind !== "review" || current.request || !proposal) return;
    const request = new AbortController();
    current.request = request;
    setStep({ ...step, saving: true, note: "Saving the destination…" });
    try {
      const { deviceKey, finishCode } = proposal;
      const { status, cleanupConfirmed } = await saveHostedConnection({ provider, deviceKey, finishCode, label: step.label }, request.signal);
      if (request.signal.aborted) return;
      if (status === 200) {
        cancel();
      } else if (status === 404 || status === 410) {
        const note = status === 404 ? "This connection expired. Start a new one." : cleanupConfirmed ?
          "Could not save the destination. Milo removed the new credential. Start a new connection to try again." :
          "Could not save the destination or confirm cleanup. Remove the connection in the provider, then start a new connection.";
        clearConnection(current);
        setStep({ kind: "choose", note });
      } else {
        const note = status === 409 ? "Settings changed. Review the destination and try Save again." :
          "Couldn’t save the destination. Your proposed destination hasn’t changed. Try Save again.";
        setStep({ ...step, note });
      }
    } finally {
      if (current.request === request) current.request = undefined;
    }
  }

  return (
    <div className="alerts-setup" ref={panel}>
      {step.kind === "choose" ? (
        <div className="alerts-connect-options">
          <div className="alerts-connect-primary">
            <p className="setting-detail">{setup.description}</p>
            <button type="button" className="button primary alerts-connect" disabled={busy} data-setup-focus onClick={start}>
              Connect with Milo
              <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5"
                strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M5 11l6-6m-4 0h4v4"/></svg>
            </button>
          </div>
          <button type="button" className="button text-button alerts-manual" disabled={busy} onClick={() => setStep({ kind: "manual" })}>
            {setup.manual}<span aria-hidden="true">›</span>
          </button>
          {forget ? (
            <div className="alerts-destination-removal">
              <button type="button" className="button alerts-forget" id={`alerts-forget-${provider}`} disabled={busy} onClick={forget}>
                <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3"
                  strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M2.5 4.5h11M6 4.5V2.75h4V4.5m2.25 0-.55 8.75H4.3l-.55-8.75M6.5 7v3.75M9.5 7v3.75"/></svg>
                Forget {label}…
              </button>
            </div>
          ) : null}
        </div>
      ) : step.kind === "manual" ? (
        <>
          <button type="button" className="button text-button" onClick={cancel}>Connection options</button>
          {children}
          {replacingActive ? <p className="setting-detail">{replacementNotice}</p> : null}
        </>
      ) : step.kind === "review" ? (
        <>
          <div className="alerts-hosted-proposal">
            <h3 className="setting-label" tabIndex={-1} data-setup-focus>Proposed destination</h3>
            <p className="setting-detail">{step.label}</p>
            <p className="setting-detail">{replacingActive ? replacementNotice : "Saving does not turn forwarding on."}</p>
          </div>
          <div className="button-row">
            <button type="button" className="button primary" aria-label={`Save ${label}`} disabled={busy || step.saving}
              onClick={saveProposal}>Save</button>
            <button type="button" className="button text-button" disabled={step.saving} onClick={cancel}>Cancel</button>
          </div>
        </>
      ) : (
        <>
          {step.kind === "waiting" ? (
            <>
              <p className="setting-detail" id={`alerts-${provider}-instructions`}>{setup.instructions} {step.mode === "production" ?
                `Copy the finish code ${provider === "telegram" ? "from the bot’s reply" : "from the confirmation page"}, then return to this tab and paste it to review. If you type it, choose Check connection.` :
                "Then come back and check the connection."}</p>
              {step.mode === "production" ? (
                <div className="secret-form alerts-form">
                  <div className="alerts-field">
                    <label className="setting-label" htmlFor={`alerts-${provider}-finish`}>Finish code</label>
                    <input id={`alerts-${provider}-finish`} ref={finishField} type="password" autoComplete="one-time-code"
                      autoCapitalize="characters" spellCheck={false} readOnly={step.checking} disabled={busy}
                      aria-describedby={`alerts-${provider}-instructions`} data-setup-focus
                      onKeyDown={event => { if (event.key === "Enter") void check(); }}
                      onPaste={event => {
                        event.preventDefault();
                        void check(event.clipboardData.getData("text/plain"));
                      }}
                      onInput={event => {
                        if (event.currentTarget !== document.activeElement) return;
                        const inputType = event.nativeEvent instanceof InputEvent ? event.nativeEvent.inputType : "";
                        if (!inputType || inputType === "insertReplacementText" || inputType === "insertFromPaste") {
                          void check(event.currentTarget.value);
                        }
                      }} />
                  </div>
                </div>
              ) : null}
            </>
          ) : null}
          <div className="button-row">
            {step.kind === "waiting" ? (
              <>
                <button type="button" className="button primary" disabled={busy} aria-disabled={step.checking}
                  data-setup-focus={step.mode === "local" ? "" : undefined} onClick={() => void check()}>Check connection</button>
                {step.openManually ? <button type="button" className="button text-button" disabled={step.checking}
                  onClick={openPairingPage}>Open {label}</button> : null}
              </>
            ) : null}
            <button type="button" className="button text-button" data-setup-focus={step.kind === "starting" ? "" : undefined} onClick={cancel}>Cancel</button>
          </div>
        </>
      )}
      <p className="setting-detail alerts-setup-note" role="status">{step.kind === "starting" ? `Opening ${label}…` :
        step.kind === "manual" ? null : step.note}</p>
    </div>
  );
}
