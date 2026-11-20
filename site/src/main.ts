import { demoLabel, frameAt, harnesses, loopLength, request, scene, still, typing } from './scene';
import { connectTerminal, element, renderFrame, renderHarness } from './terminal';

// Use representative scenes, not every beat of the animated timeline.
const paneSnapshots = ['test', 'edit', 'read', 'commit', 'usage'].map(phase => scene.find(step => step.phase === phase)!);
// How long the shell fades before the scene starts again; matches the CSS transition.
const fade = 320;

/** Starts the demo and the page's interactions. The returned function stops them and leaves the still demo behind. */
export function mountSite() {
  connectTerminal();
  const stop = new AbortController();
  const { signal } = stop;
  const onStop = (run: () => void) => signal.addEventListener('abort', run);

  const demo = element('demo');
  const reader = element('reader-pane');
  const caption = element('caption');
  const composer = element('composer-text');
  const playback = document.querySelector<HTMLButtonElement>('#demo-playback')!;
  const stillCaption = element('demo-still-caption');
  const panePicker = element('demo-panes');
  const paneOptions = panePicker.querySelectorAll('button');
  const reduceMotion = matchMedia('(prefers-reduced-motion: reduce)');
  const phone = matchMedia('(max-width: 45rem)');
  let paneSnapshot = paneSnapshots[0];
  let activeHarness = 0;
  let userPaused = false;
  let demoVisible = true;
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
    const label = playback.hidden ? stillCaption : caption;
    const words = say.replace('{agent}', harness.name);
    if (label.dataset.says !== words) {
      label.dataset.says = words;
      const [before, emphasis, after] = words.split('**');
      const strong = document.createElement('strong');
      strong.textContent = emphasis;
      label.dataset.tone = focus === 'agent' ? harness.id : focus ?? 'whole';
      label.replaceChildren(before, strong, after);
      if (!('still' in demo.dataset)) {
        caption.animate?.([{ opacity: 0, transform: 'translateY(.35em)' }, { opacity: 1, transform: 'translateY(0)' }], { duration: 340, easing: 'ease-out' });
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
    for (const animation of caption.getAnimations?.() ?? []) animation.cancel();
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
    if (userPaused || document.hidden || !demoVisible || reduceMotion.matches) {
      if (startedAt === undefined) return;
      elapsed = performance.now() - startedAt;
      startedAt = undefined;
      clearTimeout(timer);
      if ('still' in demo.dataset) return;
      demo.dataset.paused = '';
      // CSS holds keyframes, including any created by a responsive layout
      // change. The Web Animations API also catches transitions and the caption.
      for (const animation of [...(demo.getAnimations?.({ subtree: true }) ?? []), ...(caption.getAnimations?.() ?? [])]) {
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

  function labelPlayback() {
    const action = userPaused ? 'Resume demo' : 'Pause demo';
    playback.setAttribute('aria-label', action);
    playback.title = action;
    playback.toggleAttribute('data-paused', userPaused);
  }

  playback.addEventListener('click', () => {
    userPaused = !userPaused;
    labelPlayback();
    syncPlayback();
  }, { signal });
  for (const option of paneOptions) {
    option.addEventListener('click', () => {
      if (option.value === paneSnapshot.focus) return;
      paneSnapshot = paneSnapshots.find(step => step.focus === option.value)!;
      showPaneSnapshot();
    }, { signal });
  }

  function syncMotionPreference() {
    const wasStill = playback.hidden;
    const reduced = reduceMotion.matches;
    panePicker.hidden = !(reduced && phone.matches);
    playback.hidden = reduced;
    stillCaption.hidden = !reduced;
    demo.setAttribute('aria-describedby', reduced ? stillCaption.id : caption.id);
    if (panePicker.hidden) demo.setAttribute('aria-label', demoLabel);
    if (reduced || (wasStill && (userPaused || document.hidden || !demoVisible))) showStill();
    syncPlayback();
  }

  reduceMotion.addEventListener('change', syncMotionPreference, { signal });
  phone.addEventListener('change', syncMotionPreference, { signal });
  document.addEventListener('visibilitychange', syncPlayback, { signal });
  onStop(() => {
    clearTimeout(timer);
    startedAt = undefined;
    userPaused = false;
    labelPlayback();
    delete demo.dataset.paused;
    showStill();
  });

  demo.dataset.ready = '';
  syncMotionPreference();

  // Leave completed icons in the HTML; play their entrance only on the first visit.
  if ('IntersectionObserver' in window) {
    const paneStrip = document.querySelector<HTMLElement>('.feature-panes')!;
    const reveal = new IntersectionObserver(([entry], observer) => {
      if (!entry.isIntersecting) return;
      if (!reduceMotion.matches) paneStrip.classList.add('pane-beat');
      observer.disconnect();
    }, { threshold: 0.25 });
    reveal.observe(paneStrip);

    const visibility = new IntersectionObserver(([entry]) => {
      demoVisible = entry.isIntersecting;
      syncPlayback();
    });
    visibility.observe(demo);
    onStop(() => {
      reveal.disconnect();
      visibility.disconnect();
    });
  }

  for (const bubble of document.querySelectorAll<HTMLElement>('.bubbles li')) {
    let frame = 0;
    let x = 0;
    let y = 0;
    const rest = () => {
      cancelAnimationFrame(frame);
      frame = 0;
      bubble.style.removeProperty('--glint-x');
      bubble.style.removeProperty('--glint-y');
    };
    bubble.addEventListener('pointermove', event => {
      if (event.pointerType !== 'mouse' || reduceMotion.matches) return;
      x = event.clientX;
      y = event.clientY;
      if (frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        const box = bubble.getBoundingClientRect();
        if (box.width === 0 || box.height === 0) return;
        bubble.style.setProperty('--glint-x', `${20 + 60 * (x - box.left) / box.width}%`);
        bubble.style.setProperty('--glint-y', `${15 + 50 * (y - box.top) / box.height}%`);
      });
    }, { signal });
    bubble.addEventListener('pointerleave', rest, { signal });
    onStop(rest);
  }

  const feedback = document.querySelector<HTMLElement>('#copy-feedback')!;
  for (const button of document.querySelectorAll<HTMLButtonElement>('button[data-copy]')) {
    const target = document.getElementById(button.dataset.copy!)!;
    const label = button.textContent;
    let reset = 0;
    const restore = () => {
      clearTimeout(reset);
      delete button.dataset.state;
      button.textContent = label;
    };
    button.hidden = false;
    button.addEventListener('click', async () => {
      try {
        await navigator.clipboard.writeText(target.textContent);
        button.dataset.state = 'copied';
        button.textContent = 'Copied';
        feedback.textContent = 'Copied to clipboard.';
      } catch {
        const selection = window.getSelection();
        selection?.selectAllChildren(target);
        const selected = selection?.toString() === target.textContent;
        button.dataset.state = 'failed';
        button.textContent = selected ? 'Selected' : 'Failed';
        feedback.textContent = selected
          ? 'Clipboard unavailable. Copy the selected command.'
          : 'Clipboard unavailable. Select and copy the command manually.';
      }
      clearTimeout(reset);
      reset = window.setTimeout(restore, 1800);
    }, { signal });
    onStop(restore);
  }

  return () => stop.abort();
}
