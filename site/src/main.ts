import { frameAt, harnesses, loopLength, request, scene, typing } from './scene';
import { element, renderFrame } from './terminal';

const demo = element('demo');
const reader = element('reader-pane');
const caption = element('caption');
const composer = element('composer-text');
const tabs = demo.querySelectorAll<HTMLElement>('.rex-item');
const reduceMotion = matchMedia('(prefers-reduced-motion: reduce)');
// How long the agent's side fades before the scene starts again; matches the CSS transition.
const fade = 320;
let timers: number[] = [];
// Which harness's tab the scene is in, and how many passes have started.
let agent = '';
let pass = 0;

function later(ms: number, run: () => void) {
  timers.push(window.setTimeout(run, ms));
}

function stopScene() {
  for (const timer of timers) clearTimeout(timer);
  timers = [];
}

/** Draws one step: its panes, the pane it draws the eye to, and the line that says why. */
function show({ phase, focus, say: [name, said] }: typeof scene[number]) {
  renderFrame(frameAt(phase));
  demo.dataset.view = focus ?? 'whole';
  reader.hidden = focus !== 'reader';
  const words = said.replace('{agent}', agent);
  if (caption.dataset.says !== words) {
    caption.dataset.says = words;
    const pane = document.createElement('b');
    pane.textContent = name;
    caption.replaceChildren(pane, name ? ' ' : '', words);
    if (!reduceMotion.matches) {
      caption.animate([{ opacity: 0, translate: '0 .35em' }, { opacity: 1, translate: '0 0' }], { duration: 340, easing: 'ease-out' });
    }
  }
  composer.textContent = '';
  if (phase !== 'compose') return;
  const perLetter = typing.length / request.length;
  for (let letters = 1; letters <= request.length; letters++) {
    later(typing.start + letters * perLetter, () => {
      composer.textContent = request.slice(0, letters);
    });
  }
}

/** Moves the window to a harness's tab: the sidebar, the pane's title and the name in the talk. */
function enter({ tab, name }: typeof harnesses[number]) {
  agent = name;
  for (const item of tabs) item.classList.toggle('is-current', item.dataset.harness === tab);
  const tile = demo.querySelector(`[data-harness="${tab}"] .rex-tile`)!.cloneNode(true);
  element('agent-title').replaceChildren(tile, ` ${tab} ~/tidebook`);
  element('agent-name').textContent = name;
}

/**
 * Plays the scene over and over, each pass in the next harness's tab.
 * People who ask for less motion get its last frame, still.
 */
function play() {
  stopScene();
  demo.classList.remove('scene-out');
  const [first, ...rest] = scene;
  if (reduceMotion.matches) {
    enter(harnesses[0]);
    show(scene[scene.length - 1]);
    return;
  }
  enter(harnesses[pass++ % harnesses.length]);
  show(first);
  for (const step of rest) later(step.at, () => show(step));
  later(loopLength - fade, () => demo.classList.add('scene-out'));
  later(loopLength, play);
}

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
let copied = 0;

button.addEventListener('click', async () => {
  try {
    await navigator.clipboard.writeText(command.textContent);
    button.dataset.state = 'copied';
    button.textContent = 'Copied';
    feedback.textContent = 'Install command copied.';
  } catch {
    // No clipboard here: select the command so one key press copies it.
    window.getSelection()?.selectAllChildren(command);
    button.textContent = 'Selected';
    feedback.textContent = 'The clipboard is unavailable. Copy the selected command with your keyboard.';
  }
  clearTimeout(copied);
  copied = window.setTimeout(() => {
    delete button.dataset.state;
    button.textContent = 'Copy';
  }, 1800);
});
