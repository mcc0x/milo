'use client';

import { useEffect, useLayoutEffect, useMemo, useReducer, useRef, useState, useSyncExternalStore } from 'react';
import claude from '../../images/claude.png';
import codex from '../../images/codex.png';
import { demoLabel, frameAt, harnesses, loopLength, request, scene, still, typing, type Pane } from '../../scene';
import { AgentBanner, AgentChrome, AgentTranscript, History, Outline, Reader, stageOf, UsageFigure } from './panes';

type Step = typeof scene[number];

// A phone with reduced motion shows one representative scene per pane, not
// every beat of the animated timeline.
const paneSnapshots = ['test', 'edit', 'read', 'commit', 'usage'].map(phase => scene.find(step => step.phase === phase)!);
const paneNames: Record<Pane, string> = { agent: 'Agent', outline: 'Outline', reader: 'Reader', history: 'History', usage: 'Usage' };
// How long the shell fades before the scene starts again; matches the CSS transition.
const fade = 320;

type State = {
  /** Which coding agent this pass shows. */
  harness: number;
  step: Step;
  /** How much of the request has been typed into the composer. */
  typed: string;
  /** A still picture of the demo: nothing in it moves. */
  still: boolean;
  fading: boolean;
  /** The pane a phone with reduced motion has chosen. */
  snapshot: Step;
};

type Action =
  | { type: 'show'; step: Step }
  | { type: 'type'; typed: string }
  | { type: 'fade' }
  | { type: 'begin'; nextHarness: boolean }
  | { type: 'still'; picker: boolean }
  | { type: 'pick'; snapshot: Step };

// The server and the first client render draw the opening of the scene, so
// the page arrives with the demo in place and the loop picks up from there.
const opening: State = { harness: 0, step: scene[0], typed: '', still: false, fading: false, snapshot: paneSnapshots[0] };

function reduce(state: State, action: Action): State {
  switch (action.type) {
    case 'show': return { ...state, step: action.step, typed: '' };
    case 'type': return { ...state, typed: action.typed };
    case 'fade': return { ...state, fading: true };
    case 'begin': return {
      ...state,
      harness: action.nextHarness ? (state.harness + 1) % harnesses.length : state.harness,
      step: scene[0], typed: '', still: false, fading: false,
    };
    case 'still': return { ...state, step: action.picker ? state.snapshot : still, typed: '', still: true, fading: false };
    case 'pick': return { ...state, snapshot: action.snapshot, step: action.snapshot, typed: '' };
  }
}

// The typing, pane changes and fade all share one clock; a pause shifts that
// clock instead of redrawing a frame or restarting any part of the loop.
const cues: readonly { at: number; action: Action }[] = [
  ...Array.from({ length: request.length }, (_, index) => ({
    at: typing.start + (index + 1) * typing.length / request.length,
    action: { type: 'type', typed: request.slice(0, index + 1) } as const,
  })),
  ...scene.slice(1).map(step => ({ at: step.at, action: { type: 'show', step } as const })),
  { at: loopLength - fade, action: { type: 'fade' } },
  { at: loopLength, action: { type: 'begin', nextHarness: true } },
];

function useMediaQuery(query: string) {
  return useSyncExternalStore(
    notify => {
      const media = matchMedia(query);
      media.addEventListener('change', notify);
      return () => media.removeEventListener('change', notify);
    },
    () => matchMedia(query).matches,
    () => false,
  );
}

function useDocumentHidden() {
  return useSyncExternalStore(
    notify => {
      document.addEventListener('visibilitychange', notify);
      return () => document.removeEventListener('visibilitychange', notify);
    },
    () => document.hidden,
    () => false,
  );
}

/** A caption with its one emphasized phrase, written between asterisks. */
function Words({ line }: { line: string }) {
  const [before, emphasis, after] = line.split('**');
  return <>{before}<strong>{emphasis}</strong>{after}</>;
}

/** The Rex window that plays the scene, with its pane picker, caption and playback control. */
export function Demo() {
  const [state, dispatch] = useReducer(reduce, opening);
  const [userPaused, setUserPaused] = useState(false);
  const [onScreen, setOnScreen] = useState(true);
  const reduced = useMediaQuery('(prefers-reduced-motion: reduce)');
  const phone = useMediaQuery('(max-width: 45rem)');
  const hidden = useDocumentHidden();
  const playing = !(userPaused || hidden || !onScreen || reduced);
  const picker = reduced && phone;

  const demo = useRef<HTMLDivElement>(null);
  const caption = useRef<HTMLSpanElement>(null);
  const session = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const observer = new IntersectionObserver(([entry]) => setOnScreen(entry.isIntersecting));
    observer.observe(demo.current!);
    return () => observer.disconnect();
  }, []);

  // Reduced motion shows the still. So does a page that leaves reduced
  // motion, or first appears, while it cannot play: there is no moment of the
  // scene to hold. `wasReduced` starts true to cover that first appearance.
  const wasReduced = useRef(true);
  useEffect(() => {
    if (reduced || (wasReduced.current && !playing)) dispatch({ type: 'still', picker });
    wasReduced.current = reduced;
    // Playback changing on its own holds the scene where it is; see the clock below.
  }, [reduced, picker]);

  // Where the scene's clock stands while it is stopped.
  const clock = useRef({ elapsed: 0, nextCue: 0 });
  const suspended = useRef<Animation[]>([]);
  const isStill = useRef(state.still);
  useEffect(() => { isStill.current = state.still; });
  useEffect(() => {
    if (!playing) return;
    if (isStill.current) {
      clock.current = { elapsed: 0, nextCue: 0 };
      dispatch({ type: 'begin', nextHarness: false });
    } else {
      for (const animation of suspended.current) {
        if (animation.playState === 'paused') animation.play();
      }
    }
    suspended.current = [];
    let startedAt = performance.now() - clock.current.elapsed;
    let timer = 0;
    const schedule = () => {
      const cue = cues[clock.current.nextCue];
      timer = window.setTimeout(() => {
        dispatch(cue.action);
        clock.current.nextCue++;
        if (cue.action.type === 'begin') {
          clock.current.nextCue = 0;
          startedAt = performance.now();
        }
        schedule();
      }, cue.at - (performance.now() - startedAt));
    };
    schedule();
    return () => {
      clearTimeout(timer);
      clock.current.elapsed = performance.now() - startedAt;
      // CSS holds keyframes through [data-paused], including any created by a
      // responsive layout change. The Web Animations API also catches
      // transitions and the caption. A still has no animation to hold.
      if (isStill.current || !demo.current) return;
      for (const animation of [...(demo.current.getAnimations?.({ subtree: true }) ?? []), ...(caption.current?.getAnimations?.() ?? [])]) {
        if (animation.playState !== 'running') continue;
        const time = animation.currentTime;
        animation.pause();
        // Don't let an animation advance while its pause task waits for a paint.
        if (time !== null) animation.currentTime = time;
        suspended.current.push(animation);
      }
    };
  }, [playing]);

  const harness = harnesses[state.harness];
  const { phase, focus, say } = state.step;
  const frame = useMemo(() => frameAt(phase, harness.id), [phase, harness.id]);
  const stage = stageOf(phase);
  const newTab = phase === 'new-tab';
  const line = say.replace('{agent}', harness.name);
  const tone = focus === 'agent' ? harness.id : focus ?? 'whole';

  // Each new line of the caption rises into place, except in a still.
  const spoken = useRef(line);
  useEffect(() => {
    if (spoken.current === line) return;
    spoken.current = line;
    if (!state.still) {
      caption.current?.animate?.([{ opacity: 0, transform: 'translateY(.35em)' }, { opacity: 1, transform: 'translateY(0)' }], { duration: 340, easing: 'ease-out' });
    }
  }, [line, state.still]);
  useEffect(() => {
    if (!state.still) return;
    for (const animation of caption.current?.getAnimations?.() ?? []) animation.cancel();
  }, [state.still]);

  // A phone may hide the agent while another pane is in focus. Follow the
  // newly printed output when it becomes visible again, not the page itself.
  const printed = `${harness.id} ${Math.min(stage, stageOf('commit'))}`;
  const followed = useRef(printed);
  const follow = useRef(false);
  useLayoutEffect(() => {
    if (followed.current !== printed) {
      followed.current = printed;
      follow.current = true;
    }
    const pane = session.current!;
    if (follow.current && pane.clientHeight) {
      pane.scrollTop = pane.scrollHeight;
      follow.current = false;
    }
  });

  const playbackLabel = userPaused ? 'Resume demo' : 'Pause demo';
  return (
    <>
      <div className="demo-panes" id="demo-panes" role="group" aria-label="Demo panes" hidden={!picker}>
        {paneSnapshots.map(snapshot => (
          <button key={snapshot.focus} type="button" value={snapshot.focus!} aria-controls="demo" aria-pressed={snapshot === state.snapshot}
            onClick={() => dispatch({ type: 'pick', snapshot })}>{paneNames[snapshot.focus!]}</button>
        ))}
      </div>
      <div className={`terminal${state.fading ? ' scene-out' : ''}`} id="demo" ref={demo} translate="no" role="img"
        data-view={focus ?? 'whole'}
        data-still={state.still ? '' : undefined}
        data-paused={!playing && !state.still ? '' : undefined}
        data-expanded={phase === 'usage' ? '' : undefined}
        data-resized={stage >= stageOf('resize') ? '' : undefined}
        data-shell={newTab ? '' : undefined}
        aria-label={picker ? `A Rex window showing the ${state.snapshot.focus} pane` : demoLabel}
        aria-describedby={reduced ? 'demo-still-caption' : 'caption'}>
        <div className="rex-titlebar">
          <span className="window-lights"><i></i><i></i><i></i></span>
          <span className="rex-tab">tidebook</span><span className="rex-tab rex-tab-new" hidden={!newTab}>zsh</span><span>Rex</span>
        </div>
        <div className="terminal-panes">
          <p className="demo-fallback" id="demo-fallback">The interactive preview needs JavaScript.</p>
          <section className="term-pane outline">
            <div className="pane-body" id="outline-pane"><Outline frame={frame} /></div>
          </section>
          <section className="term-pane history">
            <div className="pane-body" id="history-pane"><History frame={frame} /></div>
          </section>
          <section className="term-pane usage">
            <div className="usage-strip">
              <span><img src={codex.src} width="20" height="20" alt="Codex" /><UsageFigure id="codex-usage" left={frame.usage.codex} /><small className="usage-reset term-muted">Resets in 38m</small></span>
              <span><img src={claude.src} width="20" height="20" alt="Claude" /><UsageFigure id="claude-usage" left={frame.usage.claude} /><small className="usage-reset term-muted">Resets in 1h 24m</small></span>
            </div>
          </section>
          <section className="term-pane agent" id="agent-pane" data-harness={harness.id}>
            <div className="agent-session" id="agent-session" ref={session} tabIndex={-1}>
              <header className="agent-banner" id="agent-banner"><AgentBanner harness={harness} /></header>
              <div id="agent-transcript"><AgentTranscript harness={harness} stage={stage} /></div>
            </div>
            <AgentChrome harness={harness} stage={stage} typed={state.typed} />
          </section>
          <div className="term-pane shell-pane">
            <div className="term-line shell-prompt">tidebook <span className="term-blue">main</span> › <span className="composer-caret"></span></div>
          </div>
          <section className="reader" id="reader-pane" hidden={focus !== 'reader'}>
            <header className="pane-head"><span>›_ milo view weather.ts</span><i className="pane-tools"></i></header>
            <div className="reader-content" id="reader-content"><Reader /></div>
          </section>
        </div>
      </div>
      <div className="demo-footer">
        <button className="caption-toggle" id="demo-playback" type="button" aria-controls="demo" aria-label={playbackLabel} title={playbackLabel}
          data-paused={userPaused ? '' : undefined} hidden={reduced} onClick={() => setUserPaused(paused => !paused)}>
          <span className="caption" id="caption" ref={caption} aria-hidden="true" data-tone={tone}><Words line={line} /></span>
        </button>
        {/* Without scripting the stylesheet shows this caption in place of the button, so it rests on the still's line. */}
        <p className="caption" id="demo-still-caption" role="status" data-tone={reduced ? tone : 'whole'} hidden={!reduced}><Words line={reduced ? line : still.say} /></p>
      </div>
    </>
  );
}
