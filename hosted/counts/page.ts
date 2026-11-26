import type { DayCounts, MonthCounts } from "./db";

export const RANGES = [7, 30, 90] as const;

export type DashboardView = {
  rows: DayCounts[];
  range: number;
  today: string;
  months: MonthCounts[];
  totalNewInstalls: number;
  nonce: string;
};

const escapes: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
const esc = (value: string) => value.replace(/[&<>"']/g, (c) => escapes[c]!);
const num = (value: number) => value.toLocaleString("en-US");
const monthName = (month: string) =>
  new Date(`${month}T00:00:00Z`).toLocaleDateString("en-US", { month: "long", year: "numeric", timeZone: "UTC" });
const monthDay = (day: string) =>
  new Date(`${day}T00:00:00Z`).toLocaleDateString("en-US", { month: "short", day: "numeric", timeZone: "UTC" });

const style = `
:root { color-scheme: light; --sky: #d8e7fe; --paper: #fbfdff; --ink: #182f42; --muted: #364e5c; --blue: #204e80; --line: #ceddef; --tint: #dfedff; --green: #25704f;
  font: 400 1rem/1.5 system-ui, -apple-system, sans-serif; color: var(--ink); background: var(--sky); }
*, *::before, *::after { box-sizing: border-box; }
body { margin: 0; min-block-size: 100dvh; isolation: isolate; background: #cbe5fc; -webkit-font-smoothing: antialiased; }
body::before { content: ''; position: fixed; inset: -4px; z-index: -1; pointer-events: none; border-image: url('/assets/meadow-desktop.webp') 224 0 416 fill / 14.583vw 0 27.083vw / 0 stretch; filter: blur(2px); opacity: .86; }
@media (max-width: 600px) and (orientation: portrait) { body::before { border-image-source: url('/assets/meadow-mobile.webp'); border-image-slice: 410 0 825 fill; border-image-width: 56.63vw 0 113.95vw; } }
h1, h2, p, figure, dl, dd, table { margin: 0; }
:focus-visible { outline: 2px solid #315fbc; outline-offset: 3px; }
.skip-link { position: fixed; inset: .6rem auto auto .6rem; z-index: 10; translate: 0 -160%; padding: .5rem .85rem; background: var(--paper); border: 1px solid var(--blue); border-radius: .5rem; color: var(--ink); }
.skip-link:focus { translate: 0; }
.frame { width: min(100% - 2.5rem, 66rem); margin-inline: auto; }
.topbar { display: flex; align-items: center; justify-content: space-between; gap: .75rem; padding-block: clamp(1rem, 3vh, 2.5rem) .5rem; }
.brand { display: block; width: 9rem; height: auto; }
.private { display: inline-flex; align-items: center; gap: .4rem; padding: .3rem .75rem; border: 1px solid #ffffffbf; border-radius: 999px; background: #f3f8ffd9; font-size: .82rem; font-weight: 620; color: var(--ink); white-space: nowrap; }
.private::before { content: ''; width: .45rem; height: .45rem; border-radius: 50%; background: #267659; }
.surface { display: grid; gap: .9rem; padding-block: 1rem 2.5rem; }
.opening { padding-block-start: .5rem; }
.opening h1 { font-size: clamp(1.9rem, 4vw, 2.7rem); font-weight: 770; line-height: 1.08; letter-spacing: -.065em; margin-block-end: .2rem; }
.opening p { color: var(--muted); max-width: 53ch; }
.dock { backdrop-filter: blur(16px); display: flex; gap: .15rem; width: fit-content; max-width: 100%; padding: .35rem; border: 1px solid #ffffffbf; border-radius: 1.12rem; background: #f3f8ffb8; box-shadow: inset 0 1px 0 #fffc, 0 4px 16px -8px #27445645; }
.dock a { display: flex; align-items: center; min-height: 2.75rem; padding: .35rem .9rem; border-radius: .8rem; color: var(--muted); font-size: .89rem; font-weight: 620; text-decoration: none; }
.dock a:hover { background: #ffffff91; color: var(--ink); }
.dock a[aria-current] { background: var(--ink); color: white; }
.dock a:focus-visible { outline-offset: -3px; outline-color: currentColor; }
.panel { min-width: 0; padding: clamp(1rem, 2.2vw, 1.65rem); background: rgb(246 251 255 / .8); backdrop-filter: blur(14px); border: 1px solid rgb(255 255 255 / .85); border-radius: 1.12rem; box-shadow: 0 12px 40px -28px #204e8066; }
.panel h2 { font-size: 1rem; font-weight: 730; letter-spacing: -.02em; }
.panel .note { font-size: .82rem; color: var(--muted); }
.stats { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 10rem), 1fr)); gap: .9rem; }
.stat { display: grid; gap: .15rem; }
.stat dt { font-size: .82rem; font-weight: 620; color: var(--muted); }
.stat dd { font-size: 2rem; font-weight: 770; letter-spacing: -.05em; line-height: 1.1; font-variant-numeric: tabular-nums; }
.stat .note { min-height: 1.3em; }
.charts { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 24rem), 1fr)); gap: .9rem; }
.chart { display: grid; gap: .5rem; }
.chart svg { display: block; width: 100%; height: auto; }
.bar-a { fill: var(--blue); } .bar-b { fill: var(--green); } .bar-empty { fill: var(--line); } .axis { stroke: var(--line); stroke-width: 1; }
.tablewrap { margin-block-start: .6rem; }
table { width: 100%; border-collapse: collapse; font-variant-numeric: tabular-nums; font-size: .9rem; }
caption { text-align: start; padding-block-end: .5rem; }
th, td { padding: .5rem .5rem; text-align: end; border-block-end: 1px solid var(--line); }
th:first-child, td:first-child { text-align: start; padding-inline-start: 0; }
th:last-child, td:last-child { padding-inline-end: 0; }
thead th { font-size: .78rem; font-weight: 620; color: var(--muted); }
tbody th { font-weight: 620; }
td.zero { color: #5a7083; }
.empty { display: grid; gap: .35rem; justify-items: start; }
.foot { font-size: .8rem; color: var(--muted); }
.sr-only { position: absolute; inline-size: 1px; block-size: 1px; margin: -1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
@media (max-width: 600px) { .frame { width: min(100% - 1.5rem, 66rem); } .stat dd { font-size: 1.6rem; } .stats .panel { padding: .85rem; } .dock { width: 100%; } .dock a { flex: 1; justify-content: center; padding-inline: .4rem; } th, td { padding-inline: .3rem; } }
@media (forced-colors: active) { .bar-a, .bar-b { fill: CanvasText; } .bar-empty { fill: GrayText; } .dock a[aria-current] { outline: 2px solid; } }
`;

function bars(rows: DayCounts[], pick: (row: DayCounts) => number, cls: string) {
  const width = 600;
  const height = 140;
  const max = Math.max(1, ...rows.map(pick));
  const slot = width / rows.length;
  const barWidth = Math.max(1, slot * 0.7);
  const shapes = rows.map((row, i) => {
    const value = pick(row);
    const h = value === 0 ? 2 : Math.max(2, (value / max) * (height - 8));
    return `<rect class="${value === 0 ? "bar-empty" : cls}" x="${(i * slot + (slot - barWidth) / 2).toFixed(2)}" y="${(height - h).toFixed(2)}" width="${barWidth.toFixed(2)}" height="${h.toFixed(2)}" rx="1.5"/>`;
  });
  return `<svg viewBox="0 0 ${width} ${height}" preserveAspectRatio="none" aria-hidden="true" focusable="false"><line class="axis" x1="0" x2="${width}" y1="${height - 0.5}" y2="${height - 0.5}"/>${shapes.join("")}</svg>`;
}

function chart(title: string, rows: DayCounts[], pick: (row: DayCounts) => number, cls: string, id: string) {
  const peak = rows.reduce((best, row) => (pick(row) > pick(best) ? row : best), rows[0]!);
  const summary = pick(peak) === 0 ? "No activity in this range." : `Peak ${num(pick(peak))} on ${monthDay(peak.day)}.`;
  return `<figure class="chart" aria-labelledby="${id}"><h2 id="${id}">${title}</h2>${bars(rows, pick, cls)}<p class="note">${monthDay(rows[0]!.day)} to ${monthDay(rows.at(-1)!.day)}. ${summary} The table below lists every value.</p></figure>`;
}

export function renderDashboard(view: DashboardView): string {
  const { rows, range, today, months, totalNewInstalls, nonce } = view;
  const latest = rows.at(-1)!;
  const yesterday = rows.at(-2);
  const sum = (list: DayCounts[], pick: (row: DayCounts) => number) => list.reduce((total, row) => total + pick(row), 0);
  const thisMonth = months.at(-1)!;
  const empty = rows.every((row) => row.dailyActive === 0 && row.newInstalls === 0) && months.every((m) => m.monthlyActive === 0);
  const nav = RANGES.map(
    (r) => `<a href="/?days=${r}"${r === range ? ' aria-current="page"' : ""}>${r} days</a>`,
  ).join("");

  const stats = `<dl class="stats">
<div class="stat panel"><dt>Active today (UTC)</dt><dd>${num(latest.dailyActive)}</dd><dd class="note">So far on ${monthDay(today)}</dd></div>
<div class="stat panel"><dt>Active yesterday</dt><dd>${yesterday ? num(yesterday.dailyActive) : "0"}</dd><dd class="note">${yesterday ? monthDay(yesterday.day) : ""}</dd></div>
<div class="stat panel"><dt>New installs, ${range} days</dt><dd>${num(sum(rows, (r) => r.newInstalls))}</dd><dd class="note">${num(totalNewInstalls)} since counting began</dd></div>
<div class="stat panel"><dt>Monthly active this month</dt><dd>${num(thisMonth.monthlyActive)}</dd><dd class="note">${monthName(thisMonth.month)}</dd></div>
</dl>`;

  const body = empty
    ? `<section class="panel empty" aria-labelledby="empty-title"><h2 id="empty-title">No counts in the last ${range} days</h2><p class="note">Totals appear here after a Milo installation reports a day. Nothing is shown for any single installation.</p></section>`
    : `<div class="charts"><div class="panel">${chart("Daily active", rows, (r) => r.dailyActive, "bar-a", "c-active")}</div><div class="panel">${chart("New installs", rows, (r) => r.newInstalls, "bar-b", "c-new")}</div></div>
<section class="panel" aria-labelledby="t-title"><table><caption><h2 id="t-title">Every day in range</h2></caption><thead><tr><th scope="col">Day (UTC)</th><th scope="col">Active</th><th scope="col">New</th></tr></thead><tbody>${[...rows]
        .reverse()
        .map(
          (r) =>
            `<tr><th scope="row"><time datetime="${r.day}">${r.day}</time></th><td${r.dailyActive ? "" : ' class="zero"'}>${num(r.dailyActive)}</td><td${r.newInstalls ? "" : ' class="zero"'}>${num(r.newInstalls)}</td></tr>`,
        )
        .join("")}</tbody></table></section>
<section class="panel" aria-labelledby="m-title"><table><caption><h2 id="m-title">Monthly active</h2><p class="note">One report per installation per calendar month, kept as a monthly total only.</p></caption><thead><tr><th scope="col">Month (UTC)</th><th scope="col">Active</th></tr></thead><tbody>${[...months]
        .reverse()
        .map((m) => `<tr><th scope="row">${monthName(m.month)}</th><td${m.monthlyActive ? "" : ' class="zero"'}>${num(m.monthlyActive)}</td></tr>`)
        .join("")}</tbody></table></section>`;

  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex, nofollow, noarchive"><meta name="referrer" content="no-referrer"><link rel="icon" href="/assets/favicon.png" type="image/png">
<title>Anonymous usage counts</title><style nonce="${esc(nonce)}">${style}</style></head>
<body><a class="skip-link" href="#main">Skip to counts</a>
<header class="frame topbar"><img class="brand" src="/assets/milo.webp" width="510" height="192" alt="Milo"><span class="private">Private, UTC days</span></header>
<main id="main" class="frame surface" tabindex="-1">
<div class="opening"><h1>Anonymous usage counts</h1><p>Daily totals from Milo installations. Active means an eligible interactive launch. Each report adds one to its day; no identifier is stored.</p></div>
<nav class="dock" aria-label="Date range">${nav}</nav>
${stats}
${body}
<p class="foot">Totals are additive and never revised per installation. Monthly active is a separate monthly total.</p>
</main></body></html>
`;
}
