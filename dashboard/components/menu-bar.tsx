"use client";

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

  const primaryAction = (
    <button type="button" className="button primary" id="menubar-action"
      aria-describedby="menubar-detail menubar-timing" aria-disabled={unavailable || undefined}
      onClick={() => { if (!unavailable) command("menubar", { value: "install" }); }}>
      {installed ? "Open Milo" : "Install"}
    </button>
  );
  return (
    <section className="menu-app" aria-labelledby="group-menubar">
      <header className="integration-heading">
        <h2 id="group-menubar">Menu bar app</h2>
        <p className="setting-detail" id="menubar-timing">Applies immediately</p>
        {installed ? (
          <button type="button" className="button danger" id="menubar-remove"
            aria-describedby="menubar-timing" aria-disabled={unavailable || undefined}
            onClick={() => {
              if (!unavailable) confirm("Remove menu app?",
                "This removes only the menu bar app. The Milo CLI, saved settings and current draft will be kept.",
                "Remove menu app", () => command("menubar", { value: "uninstall" }));
            }}>Uninstall</button>
        ) : primaryAction}
      </header>
      <div className="menu-app-identity">
        <img src="/favicon.png" width="42" height="42" alt="" />
        <div>
          <div className="menu-app-name"><strong>Milo</strong>
            <span className="integration-state" data-state={installed ? "on" : "off"}>{installed ? "Installed" : "Not installed"}</span>
          </div>
          <p className="setting-detail" id="menubar-detail">{installed
            ? "Opens the menu bar app and updates it if needed."
            : "Check usage and open settings from your Mac’s menu bar."}</p>
        </div>
        {installed && primaryAction}
      </div>
      <p className="setting-detail" id="menubar-status" role="status">{pending}</p>
    </section>
  );
}
