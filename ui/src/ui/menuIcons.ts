// Automatic icons for context-menu items, picked from the label (Font Awesome Free solid,
// CC BY 4.0). An item's own `icon` (a 24×24 stroke path) wins over these.
import type { IconDefinition } from '@fortawesome/free-solid-svg-icons';
import {
  faArrowDown, faArrowLeft, faArrowRight, faArrowsUpDown, faArrowUp, faBan, faChartLine, faCheckDouble, faClone, faCopy,
  faCrosshairs, faEraser, faEye, faEyeSlash, faFileAudio, faFileCirclePlus, faFillDrip, faFingerprint, faGripLinesVertical,
  faHeadphones, faKeyboard, faLayerGroup, faMagnifyingGlass, faMusic, faPaintbrush, faPaste, faPen, faPencil, faPlay, faPlus,
  faRotateLeft, faScissors, faSliders, faStopwatch, faTableCells, faTrashCan, faVolumeHigh, faVolumeXmark, faWandMagicSparkles,
  faWaveSquare, faCircle, faFolderOpen,
} from '@fortawesome/free-solid-svg-icons';

export type MenuGlyph = { vb: string; d: string };
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
export function menuGlyph(label: string): MenuGlyph | null {
  let v = cache.get(label);
  if (v === undefined) {
    const r = RULES.find(([re]) => re.test(label.trim()));
    v = r ? g(r[1]) : null;
    cache.set(label, v);
  }
  return v;
}
