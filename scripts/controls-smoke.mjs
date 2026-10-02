/** Real mouse/keyboard regression checks against a browser-only Vite session.
 * Start `pnpm --dir ui dev`, then run `pnpm --dir ui test:controls`.
 * Set AGENT_BROWSER_BIN when agent-browser is not on PATH.
 */
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { closeSync, mkdirSync, openSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const binary = process.env.AGENT_BROWSER_BIN || 'agent-browser';
const url = process.env.MYDAW_CONTROLS_URL || 'http://127.0.0.1:1420';
const session = `mydaw-controls-${process.pid}`;
const outputDir = fileURLToPath(new URL('../.debug/controls/', import.meta.url));
mkdirSync(outputDir, { recursive: true });

function command(...args) {
  const input = args[0] === 'eval' ? args.splice(1, 1)[0] : undefined;
  if (input !== undefined) args.push('--stdin');
  // On Windows the browser daemon may inherit stdout. Files avoid waiting for
  // its inherited pipe to close after the CLI has already returned a result.
  const outPath = resolve(outputDir, 'command.json');
  const errPath = resolve(outputDir, 'command.err');
  const out = openSync(outPath, 'w');
  const err = openSync(errPath, 'w');
  let failure;
  try {
    execFileSync(binary, ['--session', session, '--json', ...args], {
      stdio: [input === undefined ? 'ignore' : 'pipe', out, err], input, timeout: 30_000,
    });
  } catch (error) {
    failure = error;
  } finally {
    closeSync(out);
    closeSync(err);
  }
  const output = readFileSync(outPath, 'utf8');
  assert.ok(output.trim(), readFileSync(errPath, 'utf8') || failure?.message);
  const reply = JSON.parse(output.trim());
  assert.equal(reply.success, true, reply.error || output);
  return reply.data;
}
const evaluate = (code) => command('eval', code).result;
const click = (selector) => {
  evaluate(`window.controlsAction = ${JSON.stringify(`click ${selector}`)}`);
  return command('click', selector);
};
// agent-browser's dblclick emits the second press (clickCount=2). Include
// the first press so this tests the complete browser event sequence.
const doubleClick = (selector) => { click(selector); command('dblclick', selector); };
const press = (key) => {
  evaluate(`window.controlsAction = ${JSON.stringify(`press ${key}`)}`);
  return command('press', key);
};
const wait = () => command('wait', '1200');
const count = (selector) => evaluate(`document.querySelectorAll(${JSON.stringify(selector)}).length`);
const value = (selector) => evaluate(`document.querySelector(${JSON.stringify(selector)}).value`);
const active = (selector) => evaluate(`document.querySelector(${JSON.stringify(selector)}).classList.contains('is-active')`);
const backend = (expression) => evaluate(`(async () => {
  const backend = (await import('/src/api/backend.ts')).getBackend();
  return ${expression};
})()`);
const patternState = () => backend('await backend.getPattern()');
const clipScores = (clips) => clips.map(({ id, ...score }) => score);
function point(selector, x = 0.5, y = 0.5) {
  return evaluate(`(() => {
    const box = document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect();
    return { x: Math.round(box.x + box.width * ${x}), y: Math.round(box.y + box.height * ${y}) };
  })()`);
}
function drag(selector, dx, dy) {
  const p = point(selector);
  command('mouse', 'move', String(p.x), String(p.y));
  command('mouse', 'down', 'left');
  command('mouse', 'move', String(p.x + dx), String(p.y + dy));
  command('mouse', 'up', 'left');
}
function check(label, test) {
  test();
  const rejected = evaluate('window.controlsUnhandled');
  assert.deepEqual(rejected, [], `${label}: ${JSON.stringify(rejected)}`);
  console.log(`PASS ${label}`);
}

try {
  command('open', url);
  command('set', 'viewport', '1600', '900');
  command('wait', '.wv-chrow__label');
  assert.equal(evaluate('!!window.__TAURI_INTERNALS__'), false,
    'Run this destructive fixture test only against MockBackend, never a live project.');
  evaluate(`(() => {
    window.controlsUnhandled = [];
    window.addEventListener('unhandledrejection', event => window.controlsUnhandled.push({
      message: String(event.reason), stack: event.reason?.stack,
      action: window.controlsAction,
    }));
  })()`);

  check('instrument click stays open through mixer echoes and view navigation', () => {
    click('.wv-chrow__label');
    wait();
    assert.equal(count('.wv-plug'), 1);
    const before = evaluate('document.querySelector(".wv-plug").style.left');
    drag('.wv-plug__title', 36, 12);
    const moved = evaluate('document.querySelector(".wv-plug").style.left');
    assert.notEqual(moved, before);
    press('F9');
    click('[aria-label="Master volume"]');
    press('ArrowDown');
    wait();
    assert.equal(count('.wv-plug'), 1);
    assert.equal(evaluate('document.querySelector(".wv-plug").style.left'), moved);
    click('.wv-plug button[title="Close"]');
    assert.equal(count('.wv-plug'), 0);
  });

  check('rack tab survives the first click and opens piano roll on double-click', () => {
    press('F6');
    click('[aria-label="808 Kit step 1"]');
    doubleClick('.wv-pattab');
    assert.equal(count('.wv-roll__canvas'), 1);
    press('Control+a');
    const stateBefore = patternState();
    press('Backspace');
    assert.equal(value('select[aria-label="Piano roll snap"]'), '0');
    assert.deepEqual(patternState(), stateBefore);
    press('Backspace');
    assert.notEqual(value('select[aria-label="Piano roll snap"]'), '0');
    press('Escape');
    assert.equal(count('.wv-roll__canvas'), 0);
  });

  check('top selector and its rows accept double-click, Escape dismisses the list', () => {
    press('F5');
    doubleClick('.wv-patselect__btn');
    assert.equal(evaluate('document.querySelector(".wv-viewbar__title").textContent'), 'CHANNEL RACK');
    click('.wv-patselect__btn');
    assert.equal(count('.wv-menu--pattern'), 1);
    press('Escape');
    assert.equal(count('.wv-menu--pattern'), 0);
    press('F5');
    click('.wv-patselect__btn');
    doubleClick('.wv-menu--pattern .wv-menu__row:first-child');
    assert.equal(evaluate('document.querySelector(".wv-viewbar__title").textContent'), 'CHANNEL RACK');
  });

  check('playlist double-click edits the pattern without removing or moving its clip', () => {
    press('F5');
    click('.wv-playlist-picker .wv-pick');
    const p = point('.wv-pl__lane', 0.008, 0.5);
    command('mouse', 'move', String(p.x), String(p.y));
    command('mouse', 'down', 'left');
    command('mouse', 'up', 'left');
    assert.equal(count('.wv-clip'), 1);
    const before = patternState().playlist.clips;
    const widthBefore = evaluate('document.querySelector(".wv-clip").getBoundingClientRect().width');
    click('.wv-clip');
    assert.equal(evaluate('document.querySelector(".wv-clip").getBoundingClientRect().width'), widthBefore);
    doubleClick('.wv-clip');
    assert.equal(evaluate('document.querySelector(".wv-viewbar__title").textContent'), 'CHANNEL RACK');
    assert.deepEqual(patternState().playlist.clips, before);
    press('F5');
    assert.equal(count('.wv-clip'), 1);
    click('.wv-clip');
    press('Backspace');
    assert.equal(value('select[aria-label="Playlist snap"]'), '0');
    assert.deepEqual(patternState().playlist.clips, before);
    press('F6');
    press('F5');
    press('Backspace');
    assert.equal(value('select[aria-label="Playlist snap"]'), '3840');
    press('e');
    assert.equal(active('button[title^="Select clips"]'), true);
    press('Control+t');
    press('d');
    assert.equal(active('button[title^="Select clips"]'), true);
    press('Control+t');
    press('p');
    drag('.wv-clip', 96, 0);
    assert.notDeepEqual(patternState().playlist.clips, before);
    press('Control+z');
    // Restoring a snapshot may allocate fresh engine clip IDs.
    assert.deepEqual(clipScores(patternState().playlist.clips), clipScores(before));
  });

  check('floating instrument owns destructive keys, Delete and undo work in the playlist', () => {
    const before = patternState().playlist.clips;
    press('F6');
    click('.wv-chrow__label');
    press('F5');
    click('.wv-plug__title');
    press('Delete');
    assert.deepEqual(patternState().playlist.clips, before);
    press('Escape');
    click('.wv-clip');
    press('Delete');
    assert.equal(patternState().playlist.clips.length, 0);
    press('Control+z');
    assert.equal(patternState().playlist.clips.length, 1);
  });

  check('effect window survives bypass, mixer commands and reopening by double-click', () => {
    press('F9');
    click('[aria-label="808 Kit strip"] .wv-mixer__slot.is-add');
    click('.wv-mixer__fxmenu .wv-menu__row:first-child');
    wait();
    assert.equal(count('.wv-plug'), 1);
    click('.wv-plug .wv-plug__tools button:first-child');
    click('[aria-label="Master volume"]');
    press('ArrowDown');
    wait();
    assert.equal(count('.wv-plug'), 1);
    press('Escape');
    assert.equal(count('.wv-plug'), 0);
    doubleClick('[aria-label="808 Kit strip"] .wv-mixer__slot.is-filled');
    wait();
    assert.equal(count('.wv-plug'), 1);
    command('screenshot', resolve(outputDir, 'effect-window.png'));
    click('.wv-plug button[title="Close"]');
  });

  check('opening a new project clears stale editors and clip selection', () => {
    press('F6');
    click('.wv-chrow__label');
    click('button[aria-label^="Project and playback options"]');
    click('.wv-menu--project button.wv-menu__row');
    wait();
    assert.equal(count('.wv-plug'), 0);
    press('F5');
    assert.equal(count('.wv-clip.is-selected'), 0);
  });

  check('hosted instrument double-click toggles its GUI once', () => {
    press('F6');
    click('.wv-rack__drop');
    wait();
    const row = evaluate(`Array.from(document.querySelectorAll('.wv-modal__row'))
      .findIndex(button => button.textContent.includes('MYDAW Fixture Synth'))`);
    assert.ok(row >= 0, 'Fixture instrument must be available in MockBackend');
    click(`.wv-modal__row:nth-of-type(${row + 1})`);
    evaluate(`(() => {
      window.controlsClicks = [];
      document.addEventListener('click', event => window.controlsClicks.push({
        detail: event.detail, label: event.target.textContent, tag: event.target.tagName,
      }), true);
    })()`);
    doubleClick('.wv-chrow__label[title^="Open this instrument’s own window"]');
    wait();
    const slot = backend(`(await backend.getTracks()).tracks.find(track => track.clap)?.clap`);
    assert.equal(slot.guiOpen, true, JSON.stringify({ slot, clicks: evaluate('window.controlsClicks') }));
    click('.wv-chrow__label[title^="Open this instrument’s own window"]');
    wait();
    assert.equal(backend(`(await backend.getTracks()).tracks.find(track => track.clap)?.clap.guiOpen`), false);
  });

  assert.equal(evaluate('!!document.querySelector("vite-error-overlay")'), false);
  assert.deepEqual(command('errors').errors, []);
  command('screenshot', resolve(outputDir, 'playlist.png'));
  console.log('Controls smoke passed; screenshots in ' + outputDir);
} finally {
  command('close');
}
