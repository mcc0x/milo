"use client";

import { useRef } from "react";
import type { FormEvent } from "react";
import { useSession } from "./session";
import "./presence.css";

const details = [
  ["generic", "Generic", "No names"],
  ["session", "Session name", "May add the name of the Rex session in front."],
  ["project", "Project name", "May add the Git project’s folder name from the focused pane."],
  ["both", "Both names", "May add the project folder name, the session name, or both."],
] as const;

type Detail = (typeof details)[number][0];

// A public application ID can survive navigation without becoming a settings draft.
let applicationDraft = "";

function showsMore(next: Detail, saved: Detail) {
  return (next !== "generic" && saved === "generic") || (next === "both" && saved !== "both") ||
    (next === "session" && saved === "project") || (next === "project" && saved === "session");
}

export default function Presence() {
  const { state, paused, queue, closing, command, confirm, notify } = useSession();
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
      notify("Enter the Application ID of your own Discord application: a number of 17 to 19 digits.", true);
      input.focus();
      return;
    }
    applicationDraft = input.value = "";
    command("presence", { field: "application", value: id });
  }

  const own = presence.application_id;
  const disclosure = presence.public[presence.details];

  return (
    <section className="presence-settings" aria-labelledby="integrations-title">
      <div className="presence-preview">
        <figure className="presence-example">
          <figcaption>Activity preview</figcaption>
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
          <p className="setting-detail">{details.find(([key]) => key === presence.details)?.[2]}</p>
        </figure>
        <div className="presence-action">
          <span className="group-origin" id="presence-state">{presence.enabled ? "On" : "Off"}</span>
          <button type="button" className={`button ${presence.enabled ? "secondary" : "primary"}`}
            id="presence-change" disabled={waiting} aria-describedby="presence-state presence-status" onClick={() => {
              if (presence.enabled) command("presence", { value: "off" });
              else confirm("Turn on Discord presence?", disclosure, "Turn on", () => command("presence", { value: "on" }));
            }}>
            {presence.enabled ? "Turn off" : "Turn on…"}
          </button>
        </div>
      </div>
      <p className="setting-detail presence-status" id="presence-status">
        <strong>Last checked:</strong> {presence.status}
      </p>
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
        <p className="setting-detail" id="presence-details-detail">
          Anyone who can see your Discord activity can see what you share while a Rex app has a session open here.
        </p>
      </div>
      <div className="presence-application-options">
        <details>
          <summary>Use your own Discord app</summary>
          {own ? (
            <p className="setting-detail">Application ID: {own}. Discord shows your app’s own name and icon.</p>
          ) : (
            <>
              <p className="setting-detail" id="presence-application-detail">
                Create an app in the <a href="https://discord.com/developers/applications"
                  target="_blank" rel="noreferrer">Discord Developer Portal</a> and copy its Application ID.
                Discord shows that app’s name and icon. No bot or token is needed.
              </p>
              <form onSubmit={useOwnApplication}>
                <label className="setting-label" htmlFor="presence-application">Application ID</label>
                <div className="presence-application-input">
                  <input ref={application} type="text" id="presence-application" inputMode="numeric"
                    autoComplete="off" spellCheck={false} aria-describedby="presence-application-detail"
                    defaultValue={applicationDraft} disabled={waiting}
                    onChange={event => { applicationDraft = event.currentTarget.value.trim(); }} />
                  <button type="submit" className="button secondary" id="presence-own" disabled={waiting}>
                    Use my own
                  </button>
                </div>
              </form>
            </>
          )}
        </details>
        {own ? (
          <button type="button" className="button text-button" id="presence-official" disabled={waiting}
            onClick={() => command("presence", { field: "application", value: "official" })}>
            Use Milo’s application
          </button>
        ) : null}
      </div>
    </section>
  );
}
