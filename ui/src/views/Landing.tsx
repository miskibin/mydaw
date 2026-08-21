import { For, Show, createSignal } from 'solid-js';

import { useView } from '../shell/view';

const GITHUB_URL = 'https://github.com/miskibin/mydaw';

const proofPoints = [
  {
    eyebrow: '01 / TRANSFERABLE IDEAS',
    title: 'Patterns become arrangements',
    body: 'Sketch a loop in Rack, promote it into the Playlist, and keep the musical intent visible from first hit to final mix.',
  },
  {
    eyebrow: '02 / LESS FRICTION',
    title: 'Find sound without leaving flow',
    body: 'Browser, instruments, effects, and samples live beside the work. The next sound is a decision, not a detour.',
  },
  {
    eyebrow: '03 / HONEST ENGINE',
    title: 'A real boundary to the engine',
    body: 'WAVE keeps UI state, backend commands, and audio processing explicit. What is not ready is labelled — never faked.',
  },
];

const workflow = [
  ['CAPTURE', 'Build a pattern', 'Start with a focused loop in the Rack. Shape rhythm, notes, and sound before the arrangement asks for attention.'],
  ['COMPOSE', 'Promote the idea', 'Move the pattern into the Playlist. PAT and SONG transport modes make the intent of every playback decision obvious.'],
  ['FINISH', 'Mix with context', 'Jump to Mixer, inspect the chain, and keep the arrangement visible while the track becomes a record.'],
];

export function Landing(): ReturnType<typeof Show> {
  const view = useView();
  const [demoView, setDemoView] = createSignal<'rack' | 'playlist' | 'mixer'>('rack');

  function launchDemo(): void {
    view.go(demoView());
    window.history.pushState({}, '', '#/studio');
    window.scrollTo({ top: 0, behavior: 'smooth' });
  }

  function previewView(next: 'rack' | 'playlist' | 'mixer'): void {
    setDemoView(next);
  }

  return (
    <main class="wv-landing">
      <header class="wv-landing__nav">
        <a class="wv-landing__brand" href="#top" aria-label="WAVE Studio home">
          <span class="wv-landing__brand-mark">W</span>
          <span>WAVE <em>STUDIO</em></span>
        </a>
        <nav class="wv-landing__links" aria-label="Primary navigation">
          <a href="#why-wave">Why WAVE</a>
          <a href="#workflow">Workflow</a>
          <a href="#open-source">Open source</a>
        </nav>
        <a class="wv-landing__nav-cta" href={GITHUB_URL} target="_blank" rel="noreferrer">View on GitHub <span>↗</span></a>
      </header>

      <section class="wv-landing__hero" id="top">
        <div class="wv-landing__hero-copy">
          <p class="wv-landing__kicker"><span class="wv-landing__live-dot" /> OPEN SOURCE DIGITAL AUDIO WORKSTATION</p>
          <h1>Make the idea.<br /><span>Keep the feeling.</span></h1>
          <p class="wv-landing__lede">WAVE is a workflow-first DAW for musicians who want to move from a spark to a finished arrangement without losing the thread.</p>
          <div class="wv-landing__actions">
            <button class="wv-landing__button wv-landing__button--primary" type="button" onClick={launchDemo}>Launch interactive demo <span>→</span></button>
            <a class="wv-landing__button wv-landing__button--quiet" href={GITHUB_URL} target="_blank" rel="noreferrer">Explore the source <span>↗</span></a>
          </div>
          <div class="wv-landing__capabilities" aria-label="Core capabilities">
            <span>PLAYLIST</span><i /> <span>RACK</span><i /> <span>MIXER</span><i /> <span>PIANO ROLL</span>
          </div>
        </div>
        <div class="wv-landing__hero-art" aria-label="WAVE Studio interface preview">
          <div class="wv-landing__art-top"><span class="wv-landing__mini-brand">WAVE / SESSION 01</span><span class="wv-landing__art-status">ENGINE READY <b /></span></div>
          <div class="wv-landing__art-grid">
            <div class="wv-landing__art-browser"><span>BROWSER</span><div class="wv-landing__art-line" /><div class="wv-landing__art-line wv-landing__art-line--short" /><div class="wv-landing__art-line" /><div class="wv-landing__art-line wv-landing__art-line--accent" /></div>
            <div class="wv-landing__art-main"><div class="wv-landing__art-tabs"><b>RACK</b><span>PLAYLIST</span><span>MIXER</span></div><div class="wv-landing__art-seq"><For each={Array.from({ length: 32 })}>{(_, i) => <i class={i() % 7 === 0 || i() % 11 === 0 ? 'is-hit' : i() % 4 === 0 ? 'is-soft' : ''} />}</For></div><div class="wv-landing__art-knobs"><For each={[1, 2, 3, 4]}>{(item) => <span style={{ '--turn': `${item * 17 + 18}deg` }} />}</For></div></div>
          </div>
          <div class="wv-landing__art-footer"><span>128.00 BPM</span><span class="wv-landing__art-play">●</span><span>4 / 4 &nbsp; PAT</span><span>00:18:42</span></div>
          <div class="wv-landing__art-label">A MUSIC TOOL, NOT A MOCKUP</div>
        </div>
      </section>

      <section class="wv-landing__demo" id="demo">
        <div class="wv-landing__section-intro"><p class="wv-landing__kicker">TRY THE SHAPE OF IT</p><h2>One idea.<br /><span>Three perspectives.</span></h2><p>Switch views to feel how WAVE keeps the same musical context intact.</p></div>
        <div class="wv-landing__demo-panel">
          <div class="wv-landing__demo-head"><div><span class="wv-landing__mono">WAVE / LIVE PREVIEW</span><strong>{demoView().toUpperCase()}</strong></div><button type="button" onClick={launchDemo}>Open full studio →</button></div>
          <div class="wv-landing__demo-nav" role="tablist" aria-label="Demo views"><For each={['rack', 'playlist', 'mixer'] as const}>{(item) => <button type="button" role="tab" aria-selected={demoView() === item} class={demoView() === item ? 'is-active' : ''} onClick={() => previewView(item)}>{item}</button>}</For></div>
          <div class={`wv-landing__demo-canvas wv-landing__demo-canvas--${demoView()}`}>
            <div class="wv-landing__demo-track"><span>DRUMS</span><div class="wv-landing__demo-steps"><For each={Array.from({ length: 24 })}>{(_, i) => <i class={i() % 4 === 0 || (demoView() === 'playlist' && i() > 10 && i() < 17) ? 'is-on' : ''} />}</For></div></div>
            <div class="wv-landing__demo-track"><span>BASS</span><div class="wv-landing__demo-wave"><b /><b /><b /><b /><b /><b /><b /></div></div>
            <div class="wv-landing__demo-track"><span>TEXTURE</span><div class="wv-landing__demo-fader"><i /><i /><i /></div></div>
            <div class="wv-landing__demo-playhead" />
          </div>
          <div class="wv-landing__demo-foot"><span><b /> PLAYING PATTERN 04</span><span>Drag-free by design. Keyboard-ready.</span></div>
        </div>
      </section>

      <section class="wv-landing__proof" id="why-wave"><div class="wv-landing__wide-title"><p class="wv-landing__kicker">THE DIFFERENCE IS THE WORKFLOW</p><h2>Less interface theatre.<br /><span>More music.</span></h2></div><div class="wv-landing__proof-grid"><For each={proofPoints}>{(point) => <article><p class="wv-landing__mono">{point.eyebrow}</p><h3>{point.title}</h3><p>{point.body}</p><span class="wv-landing__card-arrow">↗</span></article>}</For></div></section>

      <section class="wv-landing__workflow" id="workflow"><div class="wv-landing__workflow-copy"><p class="wv-landing__kicker">A CLEAR PATH THROUGH THE NOISE</p><h2>From first hit<br /><span>to full song.</span></h2><p>WAVE makes the transitions between creative modes visible, reversible, and quick enough to stay in your head.</p><a class="wv-landing__text-link" href="#demo">See it in the demo →</a></div><div class="wv-landing__steps"><For each={workflow}>{(step, index) => <article><span class="wv-landing__step-index">0{index() + 1}</span><div><p class="wv-landing__mono">{step[0]}</p><h3>{step[1]}</h3><p>{step[2]}</p></div></article>}</For></div></section>

      <section class="wv-landing__open" id="open-source"><div><p class="wv-landing__kicker">BUILT IN THE OPEN</p><h2>Your DAW should<br /><span>belong to you.</span></h2></div><div class="wv-landing__open-copy"><p>WAVE is an open-source creative tool. Read the code, run it locally, shape the workflow, and help decide what comes next.</p><div class="wv-landing__open-meta"><span>MIT LICENSE</span><span>RUST + SOLID</span><span>COMMUNITY-LED</span></div><a class="wv-landing__button wv-landing__button--primary" href={GITHUB_URL} target="_blank" rel="noreferrer">Visit the repository <span>↗</span></a></div></section>
      <footer class="wv-landing__footer"><span>WAVE / STUDIO</span><span>AN OPEN SOURCE AUDIO WORKSTATION</span><a href="#top">BACK TO TOP ↑</a></footer>
    </main>
  );
}

export { GITHUB_URL };
