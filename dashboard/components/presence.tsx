"use client";

import { useRef, useState } from "react";
import type { FormEvent } from "react";
import { useSession } from "./session";
import IntegrationSwitch from "./integration-switch";
import "./presence.css";

const details = [
  ["generic", "Generic", "No names"],
  ["session", "Session name", "May add the name of the Rex session in front."],
  ["project", "Project name", "May add the Git project’s folder name from the focused pane."],
  ["both", "Both names", "May add the project folder name, the session name, or both."],
] as const;

type Detail = (typeof details)[number][0];

function showsMore(next: Detail, saved: Detail) {
  return (next !== "generic" && saved === "generic") || (next === "both" && saved !== "both") ||
    (next === "session" && saved === "project") || (next === "project" && saved === "session");
}

export default function Presence() {
  const { state, paused, queue, closing, command, confirm } = useSession();
  const appDialog = useRef<HTMLDialogElement>(null);
  const [showHint, setShowHint] = useState(false);
  const [error, setError] = useState("");
  const application = useRef<HTMLInputElement>(null);
  const presence = state.presence;
  if (!presence) return null;
  const waiting = paused || closing || queue.some(item => item.action !== "set");

  const chooseDetails = (next: Detail) => {
    if (waiting || next === presence.details) return;
    const choose = () => command("presence", { field: "details", value: next });
    if (presence.enabled && showsMore(next, presence.details)) {
      confirm("Show these names on Discord?", presence.public[next], "Show names", choose);
    } else {
      choose();
    }
  };

  function useOwnApplication(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (waiting) return;
    const input = application.current;
    if (!input) return;
    const id = input.value.trim();
    if (!/^[1-9]\d{16,18}$/.test(id)) {
      setError("Enter an Application ID of 17 to 19 digits.");
      input.focus();
      return;
    }
    appDialog.current?.close();
    command("presence", { field: "application", value: id });
  }

  const own = presence.application_id;
  const disclosure = presence.public[presence.details];

  return (
    <section className="presence-settings" aria-labelledby="integrations-title">
      <div className="presence-preview">
        <figure className="presence-example">
          <figcaption><span>Activity preview</span>{" "}
            <span className="presence-hint" onMouseEnter={() => setShowHint(true)} onMouseLeave={() => setShowHint(false)}>
              <button type="button" aria-label="What others can see" aria-describedby={showHint ? "presence-hint" : undefined}
                onFocus={() => setShowHint(true)} onClick={() => setShowHint(true)} onBlur={() => setShowHint(false)}
                onKeyDown={event => { if (event.key === "Escape") setShowHint(false); }}>!</button>
              {showHint && <span role="tooltip" id="presence-hint">{details.find(([key]) => key === presence.details)?.[2]}</span>}
            </span>
          </figcaption>
          <div className="presence-activity">
            {own ? (
              <svg className="presence-app-mark" width="36" height="36" viewBox="0 0 36 36"
                fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true">
                <rect x="8" y="8" width="20" height="20" rx="4" />
                <path d="M8 15h20m-15-3h2m2 0h2" />
              </svg>
            ) : (
              <img className="presence-app-mark" src="/favicon.png" width="36" height="36" alt="" />
            )}
            <div>
              <strong>{own ? "Your Discord app" : "Milo"}</strong>
              <p>In a Rex session</p>
            </div>
          </div>
        </figure>
        <IntegrationSwitch id="presence-change" label="Discord presence" enabled={presence.enabled} disabled={waiting}
          onClick={() => {
            if (presence.enabled) command("presence", { value: "off" });
            else confirm("Turn on Discord presence?", disclosure, "Turn on", () => command("presence", { value: "on" }));
          }}/>
      </div>
      <div className="presence-sharing">
        <h3 className="setting-label" id="presence-sharing-label">Sharing options</h3>
        <fieldset className="choice-strip" disabled={waiting} aria-labelledby="presence-sharing-label"
          aria-describedby="presence-details-detail">
          {details.map(([key, label]) => (
            <label key={key}>
              <input type="radio" name="presence-details" id={`presence-details-${key}`}
                checked={key === presence.details} onChange={() => chooseDetails(key)} />
              <span>{label}</span>
            </label>
          ))}
        </fieldset>
        <p className="setting-detail presence-audience" id="presence-details-detail">
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true">
            <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12Z"/><circle cx="12" cy="12" r="2.5"/>
          </svg>
          <span>Your Discord activity settings control who can see these details while a Rex session is open.</span>
        </p>
      </div>
      <p className="setting-detail presence-status" id="presence-status">
        <strong>Last checked:</strong> {presence.status}
      </p>
      <dialog id="presence-app-dialog" ref={appDialog} className="presence-app-dialog" aria-labelledby="presence-app-title"
        onClose={() => { setError(""); application.current?.form?.reset(); }}>
        <h3 id="presence-app-title">Discord app settings</h3>
        <p className="setting-detail" id="presence-application-detail">
          {own ? "Your app supplies the name and icon for this activity." : "Milo supplies the name and icon. To use your own, enter its public ID from the "}
          {!own && <><a href="https://discord.com/developers/applications" target="_blank" rel="noreferrer">Discord Developer Portal</a>.</>}
        </p>
        <form onSubmit={useOwnApplication}>
          <label className="setting-label" htmlFor="presence-application">Application ID</label>
          <input key={own || "official"} ref={application} id="presence-application" inputMode="numeric"
            autoComplete="off" spellCheck={false} defaultValue={own || ""} disabled={waiting}
            aria-describedby={error ? "presence-app-error" : "presence-application-detail"} aria-invalid={!!error}
            onChange={() => setError("")} />
          {error && <p role="alert" id="presence-app-error">{error}</p>}
          <div className="presence-app-actions">
            {own && <button type="button" className="button text-button" disabled={waiting}
              onClick={() => { appDialog.current?.close(); command("presence", { field: "application", value: "official" }); }}>Use Milo’s application</button>}
            <button type="button" className="button secondary" onClick={() => appDialog.current?.close()}>Cancel</button>
            <button type="submit" className="button primary" disabled={waiting}>Use my own</button>
          </div>
        </form>
      </dialog>
    </section>
  );
}
