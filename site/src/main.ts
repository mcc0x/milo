import { frameAt, harnesses, loopLength, request, scene, still, typing } from './scene';
import { element, renderFrame, renderHarness } from './terminal';

const demo = element('demo');
const reader = element('reader-pane');
const caption = element('caption');
const composer = element('composer-text');
const playback = document.querySelector<HTMLButtonElement>('#demo-playback')!;
const stillCaption = element('demo-still-caption');
const panePicker = element('demo-panes');
const paneOptions = panePicker.querySelectorAll('button');
const demoLabel = demo.getAttribute('aria-label')!;
const reduceMotion = matchMedia('(prefers-reduced-motion: reduce)');
const phone = matchMedia('(max-width: 45rem)');
// Use representative scenes, not every beat of the animated timeline.
const paneSnapshots = ['test', 'edit', 'read', 'commit', 'usage'].map(phase => scene.find(step => step.phase === phase)!);
let paneSnapshot = paneSnapshots[0];
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
  const label = panePicker.hidden ? caption : stillCaption;
  const words = say.replace('{agent}', harness.name);
  if (label.dataset.says !== words) {
    label.dataset.says = words;
    const [before, emphasis, after] = words.split('**');
    const strong = document.createElement('strong');
    strong.textContent = emphasis;
    label.dataset.tone = focus === 'agent' ? harness.id : focus ?? 'whole';
    label.replaceChildren(before, strong, after);
    if (!('still' in demo.dataset)) {
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

function showPaneSnapshot() {
  for (const option of paneOptions) {
    option.setAttribute('aria-pressed', String(option.value === paneSnapshot.focus));
  }
  demo.setAttribute('aria-label', `A Rex window showing the ${paneSnapshot.focus} pane`);
  show(paneSnapshot);
}

function showStill() {
  demo.dataset.still = '';
  demo.classList.remove('scene-out');
  for (const animation of caption.getAnimations()) animation.cancel();
  suspendedAnimations.length = 0;
  renderHarness(harnesses[activeHarness]);
  if (panePicker.hidden) show(still);
  else showPaneSnapshot();
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
for (const option of paneOptions) {
  option.addEventListener('click', () => {
    if (option.value === paneSnapshot.focus) return;
    paneSnapshot = paneSnapshots.find(step => step.focus === option.value)!;
    showPaneSnapshot();
  });
}

function syncMotionPreference() {
  const wasManual = !panePicker.hidden;
  const manual = reduceMotion.matches && phone.matches;
  panePicker.hidden = stillCaption.hidden = !manual;
  playback.hidden = manual;
  if (manual) demo.setAttribute('aria-describedby', stillCaption.id);
  else if (wasManual) {
    demo.removeAttribute('aria-describedby');
    demo.setAttribute('aria-label', demoLabel);
  }
  if (reduceMotion.matches || (wasManual && (userPaused || document.hidden))) showStill();
  syncPlayback();
}

reduceMotion.addEventListener('change', syncMotionPreference);
phone.addEventListener('change', syncMotionPreference);
document.addEventListener('visibilitychange', syncPlayback);

if (!reduceMotion.matches) beginLoop();
syncMotionPreference();

// Leave completed icons in the HTML; play their entrance only on the first visit.
const paneStrip = document.querySelector<HTMLElement>('.feature-panes')!;
const reveal = new IntersectionObserver(([entry], observer) => {
  if (!entry.isIntersecting) return;
  if (!reduceMotion.matches) paneStrip.classList.add('pane-beat');
  observer.disconnect();
}, { threshold: 0.25 });
reveal.observe(paneStrip);

for (const bubble of document.querySelectorAll<HTMLElement>('.bubbles li')) {
  bubble.addEventListener('pointermove', event => {
    if (event.pointerType !== 'mouse' || reduceMotion.matches) return;
    const box = bubble.getBoundingClientRect();
    bubble.style.setProperty('--glint-x', `${20 + 60 * (event.clientX - box.left) / box.width}%`);
    bubble.style.setProperty('--glint-y', `${15 + 50 * (event.clientY - box.top) / box.height}%`);
  });
  bubble.addEventListener('pointerleave', () => {
    bubble.style.removeProperty('--glint-x');
    bubble.style.removeProperty('--glint-y');
  });
}

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
