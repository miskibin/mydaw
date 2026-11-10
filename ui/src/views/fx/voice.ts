// Voice: what goes in and what comes out (sung note → output note, mode), and a pitch × formant
// pad. The handle is the setting (drag it = Pitch and Formant together); the small dot is what is
// applied right now (Quantize and Robot move the pitch per note). With Link the formants ride on
// the pitch shift, drawn as the diagonal through the handle.
import type { Fx } from '../../api/types';
import type { FxFrame } from '../../api/telemetry';
import { clamp } from '../../lib/format';
import { t as tr } from '../../lib/i18n';
import { hzToMidi, noteName } from '../../lib/pitch';
import { FXMETA } from '../../ui/icons';
import { MONO10, MONO9, Needle, rgba, T, type Viz, type VizExtra } from './common';

/** Pad range, semitones each side. */
export const VOICE_RANGE = 12;
const ROBOT_NOTE = 60;
const MODES = ['TRANSPOSE', 'QUANTIZE', 'ROBOT'];

/** Split of the canvas: readout | pad. */
export const voiceGeom = (W: number, H: number) => {
  const lw = Math.round(clamp(W * 0.3, 170, 240));
  const px = lw + 34, py = 26, pw = W - px - 26, ph = H - py - 30;
  return { lw, px, py, pw, ph };
};
/** Pad position of a (pitch, formant) pair in semitones. */
export function voiceToPad(pitch: number, formant: number, W: number, H: number) {
  const G = voiceGeom(W, H);
  return {
    x: G.px + (clamp(pitch, -VOICE_RANGE, VOICE_RANGE) + VOICE_RANGE) / (2 * VOICE_RANGE) * G.pw,
    y: G.py + (VOICE_RANGE - clamp(formant, -VOICE_RANGE, VOICE_RANGE)) / (2 * VOICE_RANGE) * G.ph,
  };
}
/** Semitones under a pad position (clamped to the range). */
export function padToVoice(x: number, y: number, W: number, H: number) {
  const G = voiceGeom(W, H);
  return {
    pitch: clamp((x - G.px) / G.pw * 2 * VOICE_RANGE - VOICE_RANGE, -VOICE_RANGE, VOICE_RANGE),
    formant: clamp(VOICE_RANGE - (y - G.py) / G.ph * 2 * VOICE_RANGE, -VOICE_RANGE, VOICE_RANGE),
  };
}
export const onVoicePad = (x: number, y: number, W: number, H: number) => {
  const G = voiceGeom(W, H);
  return x >= G.px - 8 && x <= G.px + G.pw + 8 && y >= G.py - 8 && y <= G.py + G.ph + 8;
};
const isRobot = (fx: Fx) => Math.round(fx.params.mode ?? 0) === 2;
/** Link rides the formants on the shift; in Robot the shift changes per sung note, so the pad shows the knob. */
const linked = (fx: Fx) => (fx.params.link ?? 0) >= 0.5 && !isRobot(fx);
/** Formant the voice gets at a pitch shift of `shift` semitones, as drawn on the pad. */
export const padFormant = (fx: Fx, shift: number) => (fx.params.formant ?? 0) + (linked(fx) ? shift : 0);
/** Params for a pad position: Robot steps by whole notes, Link keeps Formant relative to Pitch. */
export function padParams(fx: Fx, pitch: number, formantOnPad: number): { pitch: number; formant: number } {
  const p = isRobot(fx) ? Math.round(pitch) : Math.round(pitch * 10) / 10;
  const f = clamp(formantOnPad - (linked(fx) ? p : 0), -VOICE_RANGE, VOICE_RANGE);
  return { pitch: p, formant: Math.round(f * 10) / 10 };
}

const st = (v: number) => (Math.abs(v) < 0.05 ? '0.0' : (v > 0 ? '+' : '−') + Math.abs(v).toFixed(1)) + ' st';

export class VoiceViz implements Viz {
  cursor = 'default';
  hover = false;
  private seq = -1;
  private hold = -1e9;
  private inHz = 0; private outHz = 0; private conf = 0; private shift = 0;
  private shiftN = new Needle(18, 0.9);
  private confN = new Needle(10, 1);

  update(fx: Fx, a: FxFrame | null, now: number): boolean {
    if (a && a.seq !== this.seq) {
      this.seq = a.seq;
      const m = a.meters;
      this.conf = clamp(m[2], 0, 1);
      if (m[0] > 0) { this.inHz = m[0]; this.outHz = m[1]; this.shift = m[3]; this.hold = now; }
    }
    const live = now - this.hold < 250;
    if (!live) { this.conf = 0; this.shift = fx.params.pitch ?? 0; }
    const s1 = this.shiftN.step(this.shift, now), s2 = this.confN.step(this.conf, now);
    return s1 || s2 || live;
  }

  draw(g: CanvasRenderingContext2D, W: number, H: number, fx: Fx, _a: FxFrame | null, _ex: VizExtra) {
    const accent = FXMETA.voice.color, p = fx.params, on = !fx.bypass, G = voiceGeom(W, H);
    const mode = clamp(Math.round(p.mode ?? 0), 0, 2), pitch = p.pitch ?? 0, link = (p.link ?? 0) >= 0.5;
    const conf = clamp(this.confN.v, 0, 1), voiced = conf > 0.05 && this.inHz > 0;
    g.fillStyle = T.viz; g.fillRect(0, 0, W, H);

    // ---- readout (left): sung note → output note
    const cx = G.lw / 2, vis = 0.3 + 0.7 * conf;
    g.textAlign = 'center';
    g.font = MONO9; g.fillStyle = T.vizLabel;
    g.fillText(tr('c.viz.inputCaps'), cx, 22);
    g.font = '600 22px "Geist Variable", Geist, sans-serif'; g.fillStyle = rgba(T.amber, voiced ? vis : 0.3);
    g.fillText(voiced ? noteName(hzToMidi(this.inHz)) : '—', cx, 50);
    g.font = MONO10; g.fillStyle = T.fg4;
    g.fillText(voiced ? this.inHz.toFixed(1) + ' Hz' : tr('c.viz.silence'), cx, 66);
    g.strokeStyle = rgba(T.fg3, 0.5); g.lineWidth = 1.4;
    g.beginPath(); g.moveTo(cx, 76); g.lineTo(cx, 90); g.moveTo(cx - 4, 86); g.lineTo(cx, 90); g.lineTo(cx + 4, 86); g.stroke();
    g.font = MONO9; g.fillStyle = T.vizLabel;
    g.fillText(tr('c.viz.outputCaps'), cx, 106);
    const outName = !on ? 'BYPASS' : voiced ? noteName(hzToMidi(this.outHz)) : mode === 2 ? noteName(ROBOT_NOTE + Math.round(pitch)) : '—';
    g.font = `600 ${on ? Math.round(clamp(H * 0.2, 34, 56)) : 22}px "Geist Variable", Geist, sans-serif`;
    g.fillStyle = on ? rgba(accent, voiced || mode === 2 ? vis : 0.35) : T.fg4;
    g.fillText(outName, cx, 106 + Math.round(clamp(H * 0.2, 34, 56)) + 2);
    // mode + what it does now
    const my = H - 30;
    g.font = '600 10px "Geist Variable", Geist, sans-serif'; g.fillStyle = on ? accent : T.fg4;
    g.fillText(MODES[mode] + (link ? ' · LINK' : ''), cx, my);
    g.font = MONO10; g.fillStyle = T.fg4;
    g.fillText(mode === 2 ? tr('c.viz.note', { n: noteName(ROBOT_NOTE + Math.round(pitch)) }) : mode === 1 ? tr('c.viz.toSemitone', { st: st(pitch) }) : st(pitch), cx, my + 15);
    g.fillStyle = T.vizAxis; g.fillRect(G.lw, 12, 1, H - 24);

    // ---- pad (right): pitch × formant
    const { px, py, pw, ph } = G;
    const P = (pi: number, fo: number) => voiceToPad(pi, fo, W, H);
    g.fillStyle = rgba(T.fg3, 0.04); g.fillRect(px, py, pw, ph);
    g.lineWidth = 1; g.font = MONO9;
    for (let s = -VOICE_RANGE; s <= VOICE_RANGE; s += 3) {
      const a = P(s, s), major = s === 0;
      g.strokeStyle = major ? T.vizGrid2 : T.vizGrid;
      g.beginPath(); g.moveTo(Math.round(a.x) + 0.5, py); g.lineTo(Math.round(a.x) + 0.5, py + ph); g.stroke();
      g.beginPath(); g.moveTo(px, Math.round(a.y) + 0.5); g.lineTo(px + pw, Math.round(a.y) + 0.5); g.stroke();
      if (s % 6 === 0) {
        g.fillStyle = T.vizLabel; g.textAlign = 'center'; g.fillText(s > 0 ? '+' + s : String(s), a.x, py + ph + 12);
        g.textAlign = 'right'; g.fillText(s > 0 ? '+' + s : String(s), px - 5, a.y + 3);
      }
    }
    g.fillStyle = T.fg4; g.textAlign = 'left'; g.font = MONO9;
    g.fillText(mode === 2 ? tr('c.viz.noteFromC4') : 'PITCH →', px, py + ph + 24);
    g.textAlign = 'right'; g.fillText(tr('c.viz.higher'), px + pw, py + ph + 24);
    g.textAlign = 'left'; g.fillText(tr('c.viz.formantBrighter'), px + 4, py - 8);
    g.fillStyle = T.vizLabel; g.fillText(tr('c.viz.deeper'), px + 4, py + ph - 5);
    // link: formants follow the pitch along the diagonal through the knob's formant
    const fo = p.formant ?? 0;
    if (link && mode !== 2) {
      const a = P(-VOICE_RANGE, fo - VOICE_RANGE), b = P(VOICE_RANGE, fo + VOICE_RANGE);
      g.save(); g.beginPath(); g.rect(px, py, pw, ph); g.clip();
      g.setLineDash([4, 4]); g.strokeStyle = rgba(accent, 0.5); g.lineWidth = 1.2;
      g.beginPath(); g.moveTo(a.x, a.y); g.lineTo(b.x, b.y); g.stroke(); g.setLineDash([]);
      g.restore();
    }
    // live: Quantize → the shift applied now (setting + snap); Robot → where the voice is sung
    if (on && voiced && mode !== 0) {
      const lx = mode === 2 ? hzToMidi(this.inHz) - ROBOT_NOTE : this.shiftN.v;
      const l = P(lx, padFormant(fx, lx));
      g.fillStyle = T.amber; g.beginPath(); g.arc(l.x, l.y, 3.5, 0, Math.PI * 2); g.fill();
    }
    // handle: the setting
    const h = P(pitch, padFormant(fx, pitch));
    g.strokeStyle = on ? accent : T.fg4; g.lineWidth = this.hover ? 2.4 : 1.8;
    g.fillStyle = rgba(on ? accent : T.fg4, this.hover ? 0.45 : 0.28);
    g.beginPath(); g.arc(h.x, h.y, this.hover ? 8 : 7, 0, Math.PI * 2); g.fill(); g.stroke();
    // values next to the handle
    const label = (mode === 2 ? noteName(ROBOT_NOTE + Math.round(pitch)) : st(pitch)) + ' · F ' + st(padFormant(fx, pitch));
    g.font = MONO10; g.fillStyle = T.ink; g.textAlign = h.x + 16 + g.measureText(label).width > px + pw ? 'right' : 'left';
    const tx = h.x + (g.textAlign === 'right' ? -12 : 12), ty = h.y < py + 20 ? h.y + 16 : h.y - 8;
    g.fillText(label, tx, ty);
    g.textAlign = 'left';
    if (link && mode === 2) {
      g.font = MONO9; g.fillStyle = T.fg4; g.fillText(tr('c.viz.linkNote'), px + 4, py + 12);
    }
    // drive
    const drive = p.drive ?? 0;
    if (drive > 0.05) {
      g.font = MONO9; g.textAlign = 'right'; g.fillStyle = on ? T.amber : T.fg4;
      g.fillText('DRIVE ' + drive.toFixed(1) + ' dB', px + pw, py - 8); g.textAlign = 'left';
    }
  }
}
