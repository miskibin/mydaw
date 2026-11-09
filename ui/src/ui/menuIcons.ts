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
  [/^auto wokal/i, faWandMagicSparkles],
  [/^zmień nazwę|^nazwa/i, faPen],
  [/^odcisz/i, faVolumeHigh],
  [/^wycisz(?! zaznaczone)|^zakończ odsłuch/i, faVolumeXmark],
  [/^duplikuj|^klonuj/i, faClone],
  [/^kopiuj/i, faCopy],
  [/^wytnij/i, faScissors],
  [/^wklej|^schowek/i, faPaste],
  [/^podziel|^podział|^granica/i, faGripLinesVertical],
  [/^spłaszcz/i, faLayerGroup],
  [/^ukryj take/i, faEyeSlash],
  [/^pokaż take/i, faEye],
  [/^użyj całego take|^domyślny compu|^crossfade/i, faLayerGroup],
  [/^usuń|^wyczyść/i, faTrashCan],
  [/^edytuj pattern|^piano roll/i, faTableCells],
  [/^edytuj sample|^plik z dysku|^dodaj jako kanał samplera|^sampler/i, faFileAudio],
  [/^edytuj wysokość|^wysokość/i, faArrowsUpDown],
  [/^wykryj tempo|^wybij tempo|^tempo/i, faStopwatch],
  [/^edytuj punkty|^automatyz/i, faChartLine],
  [/^kanał miksera|^ścieżka klipu|^dodaj efekt/i, faSliders],
  [/^zaznacz/i, faCheckDouble],
  [/^make unique/i, faFingerprint],
  [/^odsłuchaj/i, faPlay],
  [/^solo/i, faHeadphones],
  [/^wstaw ścieżkę powyżej/i, faArrowUp],
  [/^wstaw ścieżkę poniżej/i, faArrowDown],
  [/^nowa strona|^nowy band|^dodaj|^wstaw/i, faPlus],
  [/^nowy/i, faFileCirclePlus],
  [/^przesuń w lewo/i, faArrowLeft],
  [/^przesuń w prawo/i, faArrowRight],
  [/^wypełnij/i, faFillDrip],
  [/^reset|^przywróć/i, faRotateLeft],
  [/^ustaw start/i, faCrosshairs],
  [/^rysuj|^ołówek/i, faPencil],
  [/^maluj/i, faPaintbrush],
  [/^gumka/i, faEraser],
  [/^wbudowane instrumenty|^pianino|^bas$/i, faMusic],
  [/^klawiatura|^midi/i, faKeyboard],
  [/^szukaj/i, faMagnifyingGlass],
  [/^anuluj/i, faBan],
  [/^uzbrój|^nagrywa/i, faCircle],
  [/^otwórz/i, faFolderOpen],
  [/^pokaż/i, faEye],
  [/^ukryj/i, faEyeSlash],
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
