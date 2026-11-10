import { createEffect, createSignal, For, Show } from 'solid-js';
import { be } from '../api/backend';
import type { KeyCandidate, MusicAnalysis } from '../api/types';
import { t } from '../lib/i18n';
import { NOTE_NAMES } from '../lib/pitch';
import * as A from '../state/actions';
import { findFx, insName, S } from '../state/store';
import { U } from '../state/ui';

const keyLabel = (root: number, minor: boolean) => `${NOTE_NAMES[root % 12]} ${t(minor ? 'd.music.minor' : 'd.music.major')}`;

export function MusicDialog() {
  const [loading, setLoading] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  const [res, setRes] = createSignal<MusicAnalysis | null>(null);
  const [selBpm, setSelBpm] = createSignal<number | null>(null);
  const [selKey, setSelKey] = createSignal<KeyCandidate | null>(null);
  let reqId = 0;

  createEffect(() => {
    const w = U.musicWin;
    if (!w) return;
    const id = ++reqId;
    setLoading(true);
    setErr(null);
    setRes(null);
    be()
      .musicAnalyze({ clipId: w.clipId, excludeFx: w.fxId })
      .then(r => {
        if (id !== reqId) return;
        setRes(r);
        setSelBpm(r.tempo?.bpm ?? null);
        setSelKey(r.key ? { root: r.key.root, minor: r.key.minor, score: r.key.candidates[0]?.score ?? 1 } : null);
        setLoading(false);
      })
      .catch(e => {
        if (id !== reqId) return;
        setErr(String(e));
        setLoading(false);
      });
  });

  const clip = () => {
    const cid = U.musicWin?.clipId;
    return cid != null ? S.project.clips.find(c => c.id === cid) : undefined;
  };
  const srcName = () => {
    const c = clip();
    if (c) return S.project.sources.find(s => s.id === c.ref)?.name ?? t('d.music.clip');
    const fxId = U.musicWin?.fxId;
    if (fxId != null) {
      const ff = findFx(fxId);
      return ff ? t('d.music.backing.ex', { name: insName(ff.m) }) : t('d.music.backing');
    }
    return t('d.music.allclips');
  };
  const tuneCount = () => {
    let n = 0;
    for (const ins of S.project.inserts) for (const f of ins.fx) if (f.type === 'tune') n++;
    return n;
  };

  return (
    <Show when={U.musicWin}>
      <div class="overlay" onPointerDown={A.closeMusicAnalysis}>
        <div class="dialog" style={{ width: 'min(520px, 92vw)' }} onPointerDown={e => e.stopPropagation()} data-music-dialog>
          <div class="hd">
            <span>{t('d.music.title', { name: srcName() })}</span>
            <button onClick={A.closeMusicAnalysis}>×</button>
          </div>
          <Show when={loading()}>
            <div class="row" style={{ gap: '10px', padding: '24px 0', 'justify-content': 'center', color: 'var(--fg3)' }}>
              <div class="spin" />
              <span>{t('d.music.analyzing')}</span>
            </div>
          </Show>
          <Show when={err()}>
            <div class="note err" style={{ padding: '12px 0' }}>{err()}</div>
          </Show>
          <Show when={!loading() && !err() && res()}>
            {r => (
              <div style={{ display: 'flex', 'flex-direction': 'column', gap: '14px' }}>
                {/* ---- TEMPO ---- */}
                <div style={{ padding: '10px 12px', background: 'var(--bg1)', border: '1px solid var(--line)', 'border-radius': '6px', display: 'flex', 'flex-direction': 'column', gap: '8px' }}>
                  <div class="row" style={{ 'justify-content': 'space-between' }}>
                    <span class="tag9">{t('d.music.tempo')}</span>
                    <Show when={r().tempo}>
                      {tp => <span class="mono" style={{ 'font-size': '10px', color: 'var(--fg4)' }}>{t('d.music.confidence', { n: Math.round(tp().confidence * 100) })}</span>}
                    </Show>
                  </div>
                  <Show when={r().tempo} fallback={<span class="note">{t('d.music.notempo')}</span>}>
                    {tp => (
                      <>
                        <div class="row" style={{ gap: '6px', 'flex-wrap': 'wrap' }}>
                          <For each={tp().candidates}>
                            {(c, i) => {
                              const tag = () => (i() === 0 ? t('d.music.main') : c.bpm < tp().bpm ? '½×' : '2×');
                              const on = () => selBpm() === c.bpm;
                              return (
                                <button class="smallbtn" classList={{ primary: on() }} style={{ height: '28px', padding: '0 10px', display: 'flex', 'align-items': 'center', gap: '6px' }}
                                  onClick={() => setSelBpm(c.bpm)}>
                                  <span class="mono" style={{ 'font-weight': 600, 'font-size': '13px' }}>{c.bpm.toFixed(2)}</span>
                                  <span style={{ 'font-size': '10px', opacity: 0.75 }}>{tag()}</span>
                                </button>
                              );
                            }}
                          </For>
                          <div style={{ flex: 1 }} />
                          <button class="smallbtn primary" style={{ height: '28px', padding: '0 12px' }} disabled={selBpm() == null}
                            onClick={() => { if (selBpm() != null) A.applyDetectedTempo(selBpm()!, U.musicWin?.clipId); }}>
                            {t('d.music.settempo', { bpm: (selBpm() ?? tp().bpm).toFixed(2) })}
                          </button>
                        </div>
                        <Show when={U.musicWin?.clipId != null}>
                          <span class="note">{t('d.music.fitnote')}</span>
                        </Show>
                      </>
                    )}
                  </Show>
                </div>

                {/* ---- KEY ---- */}
                <div style={{ padding: '10px 12px', background: 'var(--bg1)', border: '1px solid var(--line)', 'border-radius': '6px', display: 'flex', 'flex-direction': 'column', gap: '10px' }}>
                  <div class="row" style={{ 'justify-content': 'space-between' }}>
                    <span class="tag9">{t('d.music.key')}</span>
                    <Show when={r().key}>
                      {k => (
                        <span class="mono" style={{ 'font-size': '10px', color: 'var(--fg4)' }}>
                          {t('d.music.confidence', { n: Math.round(k().confidence * 100) })}
                          <Show when={Math.abs(k().tuningCents) >= 3}>
                            {t('d.music.tuning', { sign: k().tuningCents > 0 ? '+' : '−', n: Math.abs(Math.round(k().tuningCents)) })}
                          </Show>
                        </span>
                      )}
                    </Show>
                  </div>
                  <Show when={r().key} fallback={<span class="note">{t('d.music.nokey')}</span>}>
                    {k => (
                      <>
                        <div class="row" style={{ gap: '6px', 'flex-wrap': 'wrap' }}>
                          <For each={k().candidates}>
                            {(c, i) => {
                              const on = () => selKey()?.root === c.root && selKey()?.minor === c.minor;
                              return (
                                <button class="smallbtn" classList={{ primary: on() }} style={{ height: '28px', padding: '0 10px', display: 'flex', 'align-items': 'center', gap: '6px' }}
                                  onClick={() => setSelKey(c)}>
                                  <span style={{ 'font-weight': 600, 'font-size': '13px' }}>{keyLabel(c.root, c.minor)}</span>
                                  <span class="mono" style={{ 'font-size': '10px', opacity: 0.75 }}>{i() === 0 ? '1.' : i() + 1 + '.'}</span>
                                </button>
                              );
                            }}
                          </For>
                        </div>

                        {/* 12-note chroma bar chart */}
                        <div style={{ display: 'grid', 'grid-template-columns': 'repeat(12, 1fr)', gap: '4px', 'align-items': 'end', height: '48px', padding: '6px 4px 0' }}>
                          <For each={NOTE_NAMES}>
                            {(nm, pc) => {
                              const v = () => Math.max(0.05, k().chroma[pc()] ?? 0);
                              const isRoot = () => selKey()?.root === pc();
                              return (
                                <div style={{ display: 'flex', 'flex-direction': 'column', 'align-items': 'center', gap: '3px', height: '100%', 'justify-content': ' flex-end' }}>
                                  <div style={{ width: '100%', height: Math.round(v() * 28) + 'px', 'border-radius': '2px', background: isRoot() ? 'var(--blue)' : 'var(--line3)' }} />
                                  <span class="mono" style={{ 'font-size': '9px', color: isRoot() ? 'var(--fg-hi)' : 'var(--fg4)', 'font-weight': isRoot() ? 700 : 500 }}>{nm}</span>
                                </div>
                              );
                            }}
                          </For>
                        </div>

                        <Show when={tuneCount() > 0} fallback={<span class="note">{t('d.music.addpc')}</span>}>
                          <div class="row" style={{ gap: '8px', 'justify-content': 'flex-end', 'flex-wrap': 'wrap' }}>
                            <Show when={U.musicWin?.fxId != null}>
                              <button class="smallbtn primary" style={{ height: '28px', padding: '0 12px' }} disabled={!selKey()}
                                onClick={() => { const sk = selKey(); if (sk) A.applyKeyToTune(sk.root, sk.minor, U.musicWin!.fxId); }}>
                                {t('d.music.setthis', { key: selKey() ? keyLabel(selKey()!.root, selKey()!.minor) : '' })}
                              </button>
                            </Show>
                            <Show when={U.musicWin?.fxId == null || tuneCount() > 1}>
                              <button class="smallbtn" classList={{ primary: U.musicWin?.fxId == null }} style={{ height: '28px', padding: '0 12px' }} disabled={!selKey()}
                                onClick={() => { const sk = selKey(); if (sk) A.applyKeyToTune(sk.root, sk.minor, 'all'); }}>
                                {t('d.music.setall', { n: tuneCount() })}
                              </button>
                            </Show>
                          </div>
                        </Show>
                      </>
                    )}
                  </Show>
                </div>
              </div>
            )}
          </Show>
        </div>
      </div>
    </Show>
  );
}
