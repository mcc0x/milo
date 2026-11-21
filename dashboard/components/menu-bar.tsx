"use client";

import { SettingRow } from "./controls";
import { useSession } from "./session";

export default function MenuBar() {
  const { state, queue, connected, paused, running, closing, command, confirm } = useSession();
  const installed = state.menubar.installed;
  const operation = queue.find(item => item.action === "menubar");
  const pending = !operation ? "" : operation.value === "uninstall" ? "Removing menu app…" :
    installed ? "Opening or updating menu app…" : "Installing menu app…";
  const unavailable = !connected || paused || closing || (running && !queue.length) ||
    queue.some(item => item.action !== "set");
  if (installed === null) return null;

  return (
    <section className="settings-group" aria-labelledby="group-menubar">
      <header className="group-header group-titleline">
        <div>
          <h2 id="group-menubar">Menu bar app</h2>
          <p className="setting-detail" id="menubar-timing">Applies immediately</p>
        </div>
        <span className="setting-detail" id="menubar-status" role="status">{pending}</span>
      </header>
      <div className="group-controls">
        <SettingRow label="Milo" detailId="menubar-detail" detail={installed ?
          "Installed. Opens Milo and applies the bundled update if needed." :
          "Not installed. Install and open Milo in your Mac’s menu bar."}>
          <button type="button" className="button secondary" id="menubar-action"
            aria-describedby="menubar-detail menubar-timing" aria-disabled={unavailable || undefined}
            onClick={() => { if (!unavailable) command("menubar", { value: "install" }); }}>
            {installed ? "Open or update menu app" : "Install menu app"}
          </button>
        </SettingRow>
        {installed ? (
          <div className="setting">
            <span className="setting-detail" id="menubar-remove-detail">The Milo CLI and your settings will be kept.</span>
            <button type="button" className="button secondary" id="menubar-remove"
              aria-describedby="menubar-remove-detail menubar-timing" aria-disabled={unavailable || undefined} onClick={() => {
                if (!unavailable) confirm("Remove menu app?",
                  "This removes only the menu bar app. The Milo CLI, saved settings and current draft will be kept.",
                  "Remove menu app", () => command("menubar", { value: "uninstall" }));
              }}>
              Remove menu app…
            </button>
          </div>
        ) : null}
      </div>
    </section>
  );
}
