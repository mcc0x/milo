"use client";

import { useRef } from "react";
import { SettingRow } from "./controls";
import { useSession } from "./session";

const details = [
  ["generic", "Generic", "Only the fixed line. No names."],
  ["session", "Session name", "Also the name of the Rex session in front."],
  ["project", "Project name", "Also the folder name of the Git project in the focused pane."],
  ["both", "Both names", "Also the session name and the project’s folder name."],
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

  function useOwnApplication() {
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
    <section className="settings-group" aria-labelledby="group-presence">
      <header className="group-header">
        <div className="group-titleline">
          <h2 id="group-presence">Discord presence</h2>
          <span className="group-origin" id="presence-state">{presence.enabled ? "On" : "Off"}</span>
        </div>
        <p id="presence-public">{disclosure}</p>
      </header>
      <div className="group-controls">
        <SettingRow label="Shows" detailId="presence-details-detail"
          detail={details.find(([key]) => key === presence.details)?.[2]}>
          <fieldset className="choice-strip" disabled={waiting} aria-describedby="presence-details-detail">
            <legend className="sr-only">What Discord presence shows</legend>
            {details.map(([key, label]) => (
              <label key={key}>
                <input type="radio" name="presence-details" id={`presence-details-${key}`}
                  checked={key === presence.details} onChange={() => chooseDetails(key)} />
                <span>{label}</span>
              </label>
            ))}
          </fieldset>
        </SettingRow>
        <SettingRow label={own ? "Shown under" : "Shown under Milo’s application"}
          htmlFor={own ? undefined : "presence-application"} detailId="presence-application-detail"
          detail={own ? `Your own application, ${own}.` : presence.help}>
          <div className="setting-input">
            {own ? (
              <button type="button" className="button secondary" id="presence-official" disabled={waiting}
                aria-describedby="presence-application-detail"
                onClick={() => command("presence", { field: "application", value: "official" })}>
                Use Milo’s application
              </button>
            ) : (
              <>
                <input ref={application} type="text" id="presence-application" inputMode="numeric"
                  autoComplete="off" spellCheck={false} placeholder="Your Application ID"
                  aria-describedby="presence-application-detail" defaultValue={applicationDraft}
                  onChange={event => { applicationDraft = event.currentTarget.value.trim(); }} />
                <button type="button" className="button secondary" id="presence-own" disabled={waiting}
                  onClick={useOwnApplication}>
                  Use my own
                </button>
              </>
            )}
          </div>
        </SettingRow>
        <SettingRow label="Now" detail={presence.status} detailId="presence-status">
          <button type="button" className="button secondary" id="presence-change" disabled={waiting}
            aria-describedby="presence-status presence-public" onClick={() => {
              if (presence.enabled) command("presence", { value: "off" });
              else confirm("Turn on Discord presence?", disclosure, "Turn on", () => command("presence", { value: "on" }));
            }}>
            {presence.enabled ? "Turn off" : "Turn on…"}
          </button>
        </SettingRow>
      </div>
    </section>
  );
}
