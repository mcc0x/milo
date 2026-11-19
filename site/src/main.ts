import { frameAt, harnesses, loopLength, request, scene, typing } from './scene';
import { element, renderFrame, renderHarness } from './terminal';

const demo = element('demo');
const reader = element('reader-pane');
const caption = element('caption');
const composer = element('composer-text');
const reduceMotion = matchMedia('(prefers-reduced-motion: reduce)');
// How long the agent's side fades before the scene starts again; matches the CSS transition.
const fade = 320;
let timers: number[] = [];
let activeHarness = 0;

function later(ms: number, run: () => void) {
  timers.push(window.setTimeout(run, ms));
}

function stopScene() {
  for (const timer of timers) clearTimeout(timer);
  timers = [];
}

/** Draws one step: its panes, the pane it draws the eye to, and the line that says why. */
function show({ phase, focus, say }: typeof scene[number]) {
  const harness = harnesses[activeHarness];
  demo.dataset.view = focus ?? 'whole';
  reader.hidden = focus !== 'reader';
  renderFrame(frameAt(phase, harness.id));
  const words = say.replace('{agent}', harness.name);
  if (caption.dataset.says !== words) {
    caption.dataset.says = words;
    const [before, emphasis, after] = words.split('**');
    const strong = document.createElement('strong');
    strong.textContent = emphasis;
    caption.dataset.tone = focus === 'agent' ? harness.id : focus ?? 'whole';
    caption.replaceChildren(before, strong, after);
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

/**
 * Plays the scene over and over, each pass in the next harness's tab.
 * People who ask for less motion get its last frame, still.
 */
function play() {
  stopScene();
  demo.classList.remove('scene-out');
  renderHarness(harnesses[activeHarness]);
  const [first, ...rest] = scene;
  if (reduceMotion.matches) {
    show(scene[scene.length - 1]);
    return;
  }
  show(first);
  for (const step of rest) later(step.at, () => show(step));
  later(loopLength - fade, () => demo.classList.add('scene-out'));
  later(loopLength, () => {
    activeHarness = (activeHarness + 1) % harnesses.length;
    play();
  });
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
