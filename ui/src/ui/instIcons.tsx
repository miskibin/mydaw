import type { IconDefinition } from '@fortawesome/free-solid-svg-icons';
import {
  faDrum, faGuitar, faHeadphones, faMicrophone, faMusic, faRecordVinyl, faWandMagicSparkles, faWaveSquare,
} from '@fortawesome/free-solid-svg-icons';

/** Filled instrument glyph: viewBox + path (Font Awesome Free, CC BY 4.0, or our own 24×24 drawings). */
type Glyph = { vb: string; d: string; label: string; eo?: boolean };

const fa = (i: IconDefinition, label: string): Glyph => {
  const [w, h, , , d] = i.icon;
  return { vb: `0 0 ${w} ${h}`, d: Array.isArray(d) ? d.join(' ') : d, label };
};

/** Own drawings where Font Awesome only has Pro icons (piano, bass guitar). */
const PIANO = 'M4 9.5C4 5.91 6.91 3 10.5 3C14.09 3 17 5.91 17 9.5C17 10.74 17.78 11.83 18.96 12.22C20.78 12.83 22 14.54 22 16.46V18C22 19.66 20.66 21 19 21H7C5.34 21 4 19.66 4 18V9.5ZM6.5 14.5H7.75V17C7.75 17.41 8.09 17.75 8.5 17.75C8.91 17.75 9.25 17.41 9.25 17V14.5H10.25V17C10.25 17.41 10.59 17.75 11 17.75C11.41 17.75 11.75 17.41 11.75 17V14.5H12.75V17C12.75 17.41 13.09 17.75 13.5 17.75C13.91 17.75 14.25 17.41 14.25 17V14.5H16.25V17C16.25 17.41 16.59 17.75 17 17.75C17.41 17.75 17.75 17.41 17.75 17V14.5H19.5V18.5H6.5V14.5Z';
const BASS = 'M20.2 1.8C20.9 1.1 22.1 1.1 22.8 1.8C23.4 2.4 23.5 3.3 23 4.1L20.6 7.3C20.1 8 19.2 8.2 18.5 7.8L13.8 12.5C15.1 12.4 16.3 13.2 16.7 14.4C17.2 15.8 16.6 17.3 15.5 18.4L13.4 20.5C11.3 22.6 7.9 22.6 5.8 20.5L4.5 19.2C2.4 17.1 2.4 13.7 4.5 11.6L7.6 8.5C8.7 7.4 10.4 7 11.7 7.7C12.5 8.1 12.9 9 12.6 9.8C12.3 10.5 11.5 10.8 10.8 10.5C10.3 10.3 9.7 10.4 9.3 10.8L8.8 11.3C9.6 11.8 10.6 11.8 11.4 11.2L17.2 5.4C16.8 4.7 17 3.8 17.6 3.2L19.1 1.8H20.2ZM8.3 13.6C8.6 13.3 9.1 13.3 9.4 13.6L11.4 15.6C11.7 15.9 11.7 16.4 11.4 16.7C11.1 17 10.6 17 10.3 16.7L8.3 14.7C8 14.4 8 13.9 8.3 13.6ZM6.2 15.7C6.5 15.4 7 15.4 7.3 15.7L9.3 17.7C9.6 18 9.6 18.5 9.3 18.8C9 19.1 8.5 19.1 8.2 18.8L6.2 16.8C5.9 16.5 5.9 16 6.2 15.7Z';

/** Track icons in picker order. Ids are saved in the project, never rename them. */
export const INST_ICONS: Record<string, Glyph> = {
  vocal: fa(faMicrophone, 'Wokal'),
  guitar: fa(faGuitar, 'Gitara'),
  bass: { vb: '0 0 24 24', d: BASS, label: 'Bas', eo: true },
  piano: { vb: '0 0 24 24', d: PIANO, label: 'Pianino', eo: true },
  drums: fa(faDrum, 'Perkusja'),
  synth: fa(faWaveSquare, 'Syntezator'),
  sample: fa(faRecordVinyl, 'Sample / loop'),
  fx: fa(faWandMagicSparkles, 'Efekty'),
  bus: fa(faHeadphones, 'Odsłuch / bus'),
  other: fa(faMusic, 'Inne'),
};

export function InstIcon(props: { id?: string | null; size?: number }) {
  const g = () => (props.id ? INST_ICONS[props.id] : undefined);
  return (
    <>{g() && (
      <svg class="insticon" width={props.size ?? 14} height={props.size ?? 14} viewBox={g()!.vb} fill="currentColor" aria-label={g()!.label}>
        <path fill-rule={g()!.eo ? 'evenodd' : 'nonzero'} d={g()!.d} />
      </svg>
    )}</>
  );
}
