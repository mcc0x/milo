import { frameAt, harnesses, loopLength, request, scene, still, typing } from './scene';
import { element, renderFrame, renderHarness } from './terminal';

const demo = element('demo');
const reader = element('reader-pane');
const caption = element('caption');
const composer = element('composer-text');
const playback = document.querySelector<HTMLButtonElement>('#demo-playback')!;
const reduceMotion = matchMedia('(prefers-reduced-motion: reduce)');
// How long the shell fades before the scene starts again; matches the CSS transition.
const fade = 320;
let activeHarness = 0;
let userPaused = false;
let nextCue = 0;
let elapsed = 0;
let startedAt: number | undefined;
let timer = 0;
const suspendedAnimations: Animation[] = [];

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
}

// The typing, pane changes and fade all share one clock; a pause shifts that
// clock instead of redrawing a frame or restarting any part of the loop.
const cues = [
  ...Array.from({ length: request.length }, (_, index) => ({
    at: typing.start + (index + 1) * typing.length / request.length,
    run: () => { composer.textContent = request.slice(0, index + 1); },
  })),
  ...scene.slice(1).map(step => ({ at: step.at, run: () => show(step) })),
  { at: loopLength - fade, run: () => demo.classList.add('scene-out') },
  { at: loopLength, run: () => {
    activeHarness = (activeHarness + 1) % harnesses.length;
    beginLoop();
    startedAt = performance.now();
  } },
];

function beginLoop() {
  nextCue = 0;
  elapsed = 0;
  delete demo.dataset.still;
  demo.classList.remove('scene-out');
  renderHarness(harnesses[activeHarness]);
  show(scene[0]);
}

function showStill() {
  demo.dataset.still = '';
  demo.classList.remove('scene-out');
  for (const animation of caption.getAnimations()) animation.cancel();
  suspendedAnimations.length = 0;
  renderHarness(harnesses[activeHarness]);
  show(still);
}

function scheduleCue() {
  if (startedAt === undefined) return;
  const cue = cues[nextCue];
  timer = window.setTimeout(() => {
    nextCue++;
    cue.run();
    scheduleCue();
  }, cue.at - (performance.now() - startedAt));
}

function syncPlayback() {
  playback.disabled = reduceMotion.matches;
  playback.setAttribute('aria-label', reduceMotion.matches ? 'Demo paused for reduced motion' : userPaused ? 'Resume demo' : 'Pause demo');
  playback.title = playback.getAttribute('aria-label')!;
  playback.setAttribute('aria-pressed', String(userPaused || reduceMotion.matches));
  if (userPaused || document.hidden || reduceMotion.matches) {
    if (startedAt !== undefined) {
      elapsed = performance.now() - startedAt;
      startedAt = undefined;
      clearTimeout(timer);
    }
    if (reduceMotion.matches && !('still' in demo.dataset)) showStill();
    if ('paused' in demo.dataset) return;
    demo.dataset.paused = '';
    // CSS holds the keyframes (including any created by a responsive layout
    // change). The Web Animations API also catches transitions and the caption.
    for (const animation of [...demo.getAnimations({ subtree: true }), ...caption.getAnimations()]) {
      if (animation.playState !== 'running') continue;
      const time = animation.currentTime;
      animation.pause();
      // Don't let an animation advance while its pause task waits for a paint.
      if (time !== null) animation.currentTime = time;
      suspendedAnimations.push(animation);
    }
    return;
  }

  if (startedAt !== undefined) return;
  delete demo.dataset.paused;
  if ('still' in demo.dataset) beginLoop();
  for (const animation of suspendedAnimations) {
    if (animation.playState === 'paused') animation.play();
  }
  suspendedAnimations.length = 0;
  startedAt = performance.now() - elapsed;
  scheduleCue();
}

playback.addEventListener('click', () => {
  userPaused = !userPaused;
  syncPlayback();
});
reduceMotion.addEventListener('change', syncPlayback);
document.addEventListener('visibilitychange', syncPlayback);

if (reduceMotion.matches) {
  // A visitor who asks for less motion sees the complete demo without a loop.
  showStill();
} else {
  beginLoop();
}
syncPlayback();

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
