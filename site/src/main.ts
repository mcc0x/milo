import { frameAt, loopLength, request, scene, typing, type Phase } from './scene';
import { element, renderFrame } from './terminal';

type View = 'outline' | 'history' | 'usage' | 'reader' | 'agent';
const demo = element('demo');
const hero = demo.closest<HTMLElement>('.hero')!;
const tablist = hero.querySelector<HTMLElement>('[role="tablist"]')!;
const tabs = [...tablist.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
const reader = element('reader-pane');
const hint = element('pane-hint');
const composer = element('composer-text');
const reduceMotion = matchMedia('(prefers-reduced-motion: reduce)');
const phone = matchMedia('(max-width: 45rem)');
// How long the agent's side fades before the scene starts again; matches the CSS transition.
const fade = 320;
let current: View | null = null;
let timers: number[] = [];

function later(ms: number, run: () => void) {
  timers.push(window.setTimeout(run, ms));
}

function stopScene() {
  for (const timer of timers) clearTimeout(timer);
  timers = [];
}

function show(phase: Phase) {
  renderFrame(frameAt(phase));
  composer.textContent = '';
  if (phase !== 'compose') return;
  const perLetter = typing.length / request.length;
  for (let letters = 1; letters <= request.length; letters++) {
    later(typing.start + letters * perLetter, () => {
      composer.textContent = request.slice(0, letters);
    });
  }
}

/** Plays the scene over and over. People who ask for less motion get its last frame, still. */
function play() {
  stopScene();
  demo.classList.remove('scene-out');
  if (reduceMotion.matches) {
    show('usage');
    return;
  }
  show('compose');
  for (const { at, phase } of scene.slice(1)) later(at, () => show(phase));
  later(loopLength - fade, () => demo.classList.add('scene-out'));
  later(loopLength, play);
}

function select(view: View | null) {
  current = view;
  demo.dataset.view = view ?? 'whole';
  reader.hidden = view !== 'reader';
  hint.hidden = view !== null;
  for (const tab of tabs) {
    const active = tab.id === `${view}-tab`;
    tab.setAttribute('aria-selected', String(active));
    element(tab.getAttribute('aria-controls')!).hidden = !active;
  }
}

function focusTab(tab: HTMLButtonElement) {
  for (const other of tabs) other.tabIndex = other === tab ? 0 : -1;
  tab.focus({ preventScroll: true });
}

// Choosing the chosen pane again returns to the whole tab.
for (const tab of tabs) {
  const view = tab.id.replace('-tab', '') as View;
  tab.addEventListener('click', () => {
    focusTab(tab);
    select(current === view ? null : view);
  });
}

tablist.addEventListener('keydown', event => {
  const visible = phone.matches ? tabs : tabs.filter(tab => tab.id !== 'agent-tab');
  const index = visible.indexOf(document.activeElement as HTMLButtonElement);
  let next: HTMLButtonElement;
  switch (event.key) {
    case 'ArrowLeft': next = visible[(index - 1 + visible.length) % visible.length]; break;
    case 'ArrowRight': next = visible[(index + 1) % visible.length]; break;
    case 'Home': next = visible[0]; break;
    case 'End': next = visible[visible.length - 1]; break;
    default: return;
  }
  event.preventDefault();
  focusTab(next);
});

hero.addEventListener('keydown', event => {
  if (event.key !== 'Escape' || !current) return;
  event.preventDefault();
  const tab = tabs.find(tab => tab.id === `${current}-tab`)!;
  const focus = document.activeElement;
  if (reader.contains(focus) || element(tab.getAttribute('aria-controls')!).contains(focus)) focusTab(tab);
  select(null);
});

element('close-reader').addEventListener('click', () => {
  select(null);
  focusTab(tabs.find(tab => tab.id === 'reader-tab')!);
});

// The Agent choice exists only where one pane is shown at a time.
phone.addEventListener('change', event => {
  if (event.matches) return;
  if (current === 'agent') select(null);
  const agent = element('agent-tab');
  if (agent.tabIndex !== 0) return;
  if (document.activeElement === agent) focusTab(tabs[0]);
  else {
    agent.tabIndex = -1;
    tabs[0].tabIndex = 0;
  }
});

reduceMotion.addEventListener('change', play);
// Nothing plays to a hidden page; it starts from the top when the person comes back.
document.addEventListener('visibilitychange', () => {
  if (document.hidden) stopScene();
  else play();
});
play();

const button = document.querySelector<HTMLButtonElement>('#copy-command')!;
const command = document.querySelector<HTMLElement>('#install-command')!;
const feedback = document.querySelector<HTMLElement>('#copy-feedback')!;

button.addEventListener('click', async () => {
  try {
    await navigator.clipboard.writeText(command.textContent);
    button.textContent = 'Copied';
    feedback.textContent = 'Install command copied.';
  } catch {
    const range = document.createRange();
    range.selectNodeContents(command);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
    button.textContent = 'Selected';
    feedback.textContent = 'The clipboard is unavailable. Copy the selected command with your keyboard.';
  }
});
