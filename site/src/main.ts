import { frameAt, loopLength, request, scene, typing } from './scene';
import { element, renderFrame } from './terminal';

const demo = element('demo');
const reader = element('reader-pane');
const caption = element('caption');
const composer = element('composer-text');
const reduceMotion = matchMedia('(prefers-reduced-motion: reduce)');
// How long the agent's side fades before the scene starts again; matches the CSS transition.
const fade = 320;
let timers: number[] = [];

function later(ms: number, run: () => void) {
  timers.push(window.setTimeout(run, ms));
}

function stopScene() {
  for (const timer of timers) clearTimeout(timer);
  timers = [];
}

/** Draws one step: its panes, the pane it draws the eye to, and the line that says why. */
function show({ phase, focus, caption: words }: typeof scene[number]) {
  renderFrame(frameAt(phase));
  demo.dataset.view = focus ?? 'whole';
  reader.hidden = focus !== 'reader';
  caption.textContent = words;
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

// Each card below shows its pane as it stands when the scene has finished.
renderFrame(frameAt('rest'));
for (const shot of document.querySelectorAll<HTMLElement>('[data-shot]')) {
  const copy = element(shot.dataset.shot!).cloneNode(true) as HTMLElement;
  for (const node of copy.querySelectorAll('[id]')) node.removeAttribute('id');
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
