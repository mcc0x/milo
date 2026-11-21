"use client";

import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { useSession, type AlertDestination } from "./session";

const providerSetup = {
  telegram: {
    url: /^https:\/\/t\.me\/[A-Za-z0-9_]{5,32}\?start=[A-Za-z0-9_-]{43}$/,
    description: "Send alerts to your private chat with Milo’s Telegram bot.",
    instructions: "Press Start in Milo’s Telegram chat, then come back and check the connection.",
    manual: "Use your own bot",
  },
  discord: {
    url: /^http:\/\/(?:127\.0\.0\.1|localhost):6120\/discord\/start\/[A-Za-z0-9_-]{43}$/,
    description: "Let Milo post alerts to a Discord channel you choose.",
    instructions: "Choose a channel and authorize Milo in Discord, then come back and check the connection.",
    manual: "Use your own webhook",
  },
} satisfies Record<AlertDestination, {url: RegExp; description: string; instructions: string; manual: string}>;

type Step =
  | { kind: "choose"; note?: string }
  | { kind: "starting" }
  | { kind: "waiting"; note?: string; checking?: boolean; openManually: boolean }
  | { kind: "review"; label: string }
  | { kind: "manual" };

type Connection = {
  request?: AbortController;
  pair?: { deviceKey: string; url: string };
  proposal?: { credential: string; chat: string };
};

function clearConnection(connection: Connection) {
  connection.request?.abort();
  connection.request = undefined;
  connection.pair = undefined;
  connection.proposal = undefined;
}

export default function HostedAlertsSetup({ provider, label, busy, replacingActive, save, forget, children }: {
  provider: AlertDestination;
  label: string;
  busy: boolean;
  replacingActive: boolean;
  save: (credential: string, chat: string) => void;
  forget?: () => void;
  children: ReactNode;
}) {
  const { hostedConnect } = useSession();
  const [step, setStep] = useState<Step>({ kind: "choose" });
  // React state and the DOM get only the review label, never either key or the webhook.
  const connection = useRef<Connection>({});
  const panel = useRef<HTMLDivElement>(null);
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
      if (status !== 200 || !data || typeof data !== "object" ||
        !("deviceKey" in data) || typeof data.deviceKey !== "string" || !data.deviceKey ||
        !("url" in data) || typeof data.url !== "string" || !setup.url.test(data.url)) {
        throw new Error();
      }
      current.pair = { deviceKey: data.deviceKey, url: data.url };
      const opened = popup && !popup.closed;
      if (opened) {
        popup?.location.replace(data.url);
        popup = null;
      }
      setStep({ kind: "waiting", openManually: !opened, note: opened ? undefined : `Use Open ${label} to continue.` });
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

  async function check() {
    const current = connection.current;
    if (busy || step.kind !== "waiting" || !current.pair || current.request) return;
    const { deviceKey } = current.pair;
    const request = new AbortController();
    current.request = request;
    setStep({ ...step, note: "Checking the connection…", checking: true });
    try {
      const { status, data } = await hostedConnect({ action: "claim", deviceKey }, request.signal);
      if (request.signal.aborted) return;
      if (status === 202 && data && typeof data === "object" && "pending" in data && data.pending === true) {
        setStep({ ...step, note: `Not connected yet. Finish in ${label} and check again.` });
      } else if (status === 404) {
        current.pair = undefined;
        setStep({ kind: "choose", note: "This connection expired or was already used. Start a new one or use your own credentials." });
      } else if (status === 200 && data && typeof data === "object" &&
        "provider" in data && data.provider === provider &&
        "credential" in data && typeof data.credential === "string" && data.credential &&
        "chat" in data && typeof data.chat === "string" &&
        (provider === "telegram" ? data.credential.startsWith("milorelay_") && /^[0-9]+$/.test(data.chat) : data.chat === "") &&
        "label" in data && typeof data.label === "string" && data.label.trim() &&
        !data.label.includes(data.credential) && !data.label.includes(deviceKey)) {
        current.proposal = { credential: data.credential, chat: data.chat };
        current.pair = undefined;
        setStep({ kind: "review", label: data.label });
      } else {
        throw new Error();
      }
    } catch {
      if (!request.signal.aborted) setStep({ ...step, note: "Couldn’t check the connection. Try again, or cancel and use your own credentials." });
    } finally {
      if (current.request === request) current.request = undefined;
    }
  }

  function saveProposal() {
    const proposal = connection.current.proposal;
    if (busy || !proposal) return;
    cancel();
    save(proposal.credential, proposal.chat);
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
            <button type="button" className="button primary" aria-label={`Save ${label}`} disabled={busy} onClick={saveProposal}>Save</button>
            <button type="button" className="button text-button" onClick={cancel}>Cancel</button>
          </div>
        </>
      ) : (
        <>
          {step.kind === "waiting" ? <p className="setting-detail">{setup.instructions}</p> : null}
          <div className="button-row">
            {step.kind === "waiting" ? (
              <>
                <button type="button" className="button primary" disabled={busy} aria-disabled={step.checking}
                  data-setup-focus onClick={check}>Check connection</button>
                {step.openManually ? <button type="button" className="button text-button" disabled={step.checking}
                  onClick={openPairingPage}>Open {label}</button> : null}
              </>
            ) : null}
            <button type="button" className="button text-button" data-setup-focus={step.kind === "starting" ? "" : undefined} onClick={cancel}>Cancel</button>
          </div>
        </>
      )}
      <p className="setting-detail alerts-setup-note" role="status">{step.kind === "starting" ? `Opening ${label}…` :
        step.kind === "choose" || step.kind === "waiting" ? step.note : null}</p>
    </div>
  );
}
