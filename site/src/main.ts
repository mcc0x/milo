import { frameAt, loopLength, request, scene, typing, type Pane, type Phase } from './scene';
import { element, renderFrame } from './terminal';

const demo = element('demo');
const reader = element('reader-pane');
const caption = element('caption');
const composer = element('composer-text');
const mascot = document.querySelector<HTMLElement>('.mascot')!;
const reduceMotion = matchMedia('(prefers-reduced-motion: reduce)');
// How long the agent's side fades before the scene starts again; matches the CSS transition.
const fade = 320;
let timers: number[] = [];
let spot: Pane | null = null;

function later(ms: number, run: () => void) {
  timers.push(window.setTimeout(run, ms));
}

function stopScene() {
  for (const timer of timers) clearTimeout(timer);
  timers = [];
}

/** Draws one step: its panes, the pane it draws the eye to, and the line that says why. */
function show({ phase, focus, say: [name, words] }: typeof scene[number]) {
  renderFrame(frameAt(phase));
  demo.dataset.view = focus ?? 'whole';
  reader.hidden = focus !== 'reader';
  if (caption.dataset.says !== words) {
    caption.dataset.says = words;
    const pane = document.createElement('b');
    pane.textContent = name;
    caption.replaceChildren(pane, name ? ' ' : '', words);
    if (!reduceMotion.matches) {
      caption.animate([{ opacity: 0, translate: '0 .35em' }, { opacity: 1, translate: '0 0' }], { duration: 340, easing: 'ease-out' });
    }
  }
  // The cat looks over each time one of its panes has something to show.
  if (focus !== spot && focus && focus !== 'agent' && !reduceMotion.matches) {
    mascot.animate(
      [{ rotate: '0deg' }, { rotate: '-8deg', offset: .3 }, { rotate: '3deg', offset: .65 }, { rotate: '0deg' }],
      { duration: 700, easing: 'ease-in-out' },
    );
  }
  spot = focus;
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
  const [first, ...rest] = scene;
  if (reduceMotion.matches) {
    show(scene[scene.length - 1]);
    return;
  }
  show(first);
  for (const step of rest) later(step.at, () => show(step));
  later(loopLength - fade, () => demo.classList.add('scene-out'));
  later(loopLength, play);
}

// Each card shows its pane at the moment that says most about it.
for (const shot of document.querySelectorAll<HTMLElement>('[data-shot]')) {
  renderFrame(frameAt(shot.dataset.frame as Phase));
  const copy = element(shot.dataset.shot!).cloneNode(true) as HTMLElement;
  for (const node of copy.querySelectorAll('[id]')) node.removeAttribute('id');
  // A still picture: nothing in it has "just" changed.
  for (const node of copy.querySelectorAll('.term-lit')) node.classList.remove('term-lit');
  shot.replaceChildren(...copy.childNodes);
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
