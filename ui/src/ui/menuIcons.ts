// Automatic icons for context-menu items, picked from the label (Font Awesome Free solid,
// CC BY 4.0). An item's own `icon` (a 24×24 stroke path) wins over these.
import type { IconDefinition } from '@fortawesome/free-solid-svg-icons';
import {
  faArrowDown, faArrowLeft, faArrowRight, faArrowsUpDown, faArrowUp, faBan, faChartLine, faCheck, faCheckDouble, faClone, faCopy,
  faCrosshairs, faEraser, faEye, faEyeSlash, faFileAudio, faFileCirclePlus, faFillDrip, faFingerprint, faGripLinesVertical,
  faHeadphones, faKeyboard, faLayerGroup, faMagnifyingGlass, faMusic, faPaintbrush, faPaste, faPen, faPencil, faPlay, faPlus,
  faRotateLeft, faScissors, faSliders, faStopwatch, faTableCells, faTrashCan, faVolumeHigh, faVolumeXmark, faWandMagicSparkles,
  faWaveSquare, faCircle, faFolderOpen, faBell, faBolt, faCircleDot, faCloud, faDrumSteelpan, faGuitar, faHandsClapping, faRadio,
  faStairs, faWind,
} from '@fortawesome/free-solid-svg-icons';
import { INST_ICONS } from './instIcons';

/** `eo`: the path uses the even-odd fill rule (our own 24×24 instrument drawings). */
export type MenuGlyph = { vb: string; d: string; eo?: boolean };
const g = (i: IconDefinition): MenuGlyph => {
  const [w, h, , , d] = i.icon;
  return { vb: `0 0 ${w} ${h}`, d: Array.isArray(d) ? d.join(' ') : d };
};

// First match wins: put specific phrases before generic words.
const RULES: [RegExp, IconDefinition][] = [
  [/^auto wokal|^auto[ -]?vocal/i, faWandMagicSparkles],
  [/^zmień nazwę|^nazwa|^rename|^name/i, faPen],
  [/^odcisz|^unmute/i, faVolumeHigh],
  [/^wycisz(?! zaznaczone)|^zakończ odsłuch|^mute(?! selected)|^end preview|^stop preview/i, faVolumeXmark],
  [/^duplikuj|^klonuj|^duplicate|^clone/i, faClone],
  [/^kopiuj|^copy/i, faCopy],
  [/^wytnij|^cut/i, faScissors],
  [/^wklej|^schowek|^paste|^clipboard/i, faPaste],
  [/^podziel|^podział|^granica|^split|^boundary/i, faGripLinesVertical],
  [/^spłaszcz|^flatten/i, faLayerGroup],
  [/^ukryj take|^hide take/i, faEyeSlash],
  [/^pokaż take|^show take/i, faEye],
  [/^użyj całego take|^domyślny compu|^crossfade|^use (the )?whole take|^default comp/i, faLayerGroup],
  [/^usuń|^wyczyść|^delete|^remove|^clear/i, faTrashCan],
  [/^edytuj pattern|^piano roll|^edit pattern/i, faTableCells],
  [/^edytuj sample|^plik z dysku|^dodaj jako kanał samplera|^sampler|^edit sample|^file from disk|^add as sampler channel/i, faFileAudio],
  [/^edytuj wysokość|^wysokość|^edit pitch|^pitch/i, faArrowsUpDown],
  [/^wykryj tempo|^wybij tempo|^tempo|^detect tempo|^tap tempo/i, faStopwatch],
  [/^edytuj punkty|^automatyz|^edit points|^automation/i, faChartLine],
  [/^kanał miksera|^ścieżka klipu|^dodaj efekt|^mixer channel|^(clip )?track( of (the )?clip)?$|^clip track|^add effect|^add fx/i, faSliders],
  [/^zaznacz|^select/i, faCheckDouble],
  [/^make unique/i, faFingerprint],
  [/^odsłuchaj|^preview|^audition/i, faPlay],
  [/^solo/i, faHeadphones],
  [/^wstaw ścieżkę powyżej|^insert track above/i, faArrowUp],
  [/^wstaw ścieżkę poniżej|^insert track below/i, faArrowDown],
  [/^nowa strona|^nowy band|^dodaj|^wstaw|^new page|^new band|^add|^insert/i, faPlus],
  [/^nowy|^new/i, faFileCirclePlus],
  [/^przesuń w lewo|^move left/i, faArrowLeft],
  [/^przesuń w prawo|^move right/i, faArrowRight],
  [/^wypełnij|^fill/i, faFillDrip],
  [/^reset|^przywróć|^restore/i, faRotateLeft],
  [/^ustaw start|^set start/i, faCrosshairs],
  [/^rysuj|^ołówek|^draw|^pencil/i, faPencil],
  [/^maluj|^paint/i, faPaintbrush],
  [/^gumka|^eraser/i, faEraser],
  [/^wbudowane instrumenty|^pianino|^built-in instruments|^piano/i, faMusic],
  [/^klawiatura|^midi|^keyboard/i, faKeyboard],
  [/^szukaj|^search/i, faMagnifyingGlass],
  [/^anuluj|^cancel/i, faBan],
  [/^uzbrój|^nagrywa|^arm|^record/i, faCircle],
  [/^otwórz|^open/i, faFolderOpen],
  [/^pokaż|^show/i, faEye],
  [/^ukryj|^hide/i, faEyeSlash],
  [/wave|^sample/i, faWaveSquare],
];

const cache = new Map<string, MenuGlyph | null>();
/** The mark of a toggle item that is on. */
export const CHECK_GLYPH: MenuGlyph = g(faCheck);
export function menuGlyph(label: string): MenuGlyph | null {
  let v = cache.get(label);
  if (v === undefined) {
    const r = RULES.find(([re]) => re.test(label.trim()));
    v = r ? g(r[1]) : null;
    cache.set(label, v);
  }
  return v;
}

// ---------------------------------------------------------------- instrument picker
// Keyed by engine ids (synth categories, built-in instruments), never by translated labels.

const SYNTH_CATEGORY: Record<string, MenuGlyph> = {
  '808': g(faCircleDot), // the speaker cone of a sub hit
  'Bas': INST_ICONS.bass,
  'Pluck': g(faGuitar),
  'Lead': g(faBolt),
  'Pad': g(faCloud),
  'Klawisze': INST_ICONS.piano,
  'Dzwonki': g(faBell),
  'Dęte/Smyczki': g(faWind),
  'Arp': g(faStairs),
  'Lo-fi': g(faRadio),
  'FX': INST_ICONS.fx,
};
/** A synth preset category's glyph (its presets show it too); a category added later gets the synth wave. */
export const synthCategoryGlyph = (category: string): MenuGlyph => SYNTH_CATEGORY[category] ?? INST_ICONS.synth;

const BUILTIN: Record<string, MenuGlyph> = {
  kick: INST_ICONS.drums,
  clap: g(faHandsClapping),
  hat: g(faDrumSteelpan),
  bass: INST_ICONS.bass,
};
/** Glyph for a built-in instrument id (kick, clap, hat, bass). */
export const builtinGlyph = (inst: string): MenuGlyph => BUILTIN[inst] ?? INST_ICONS.drums;

/** Other instrument-picker rows. */
export const PICK_GLYPH = {
  emptySynth: INST_ICONS.synth,
  keys: INST_ICONS.piano,
  source: INST_ICONS.sample,
} satisfies Record<string, MenuGlyph>;
