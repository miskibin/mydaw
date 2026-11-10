import type { en } from './en';
import { pl as a } from './part-a';
import { pl as b } from './part-b';
import { pl as c } from './part-c';
import { pl as d } from './part-d';

export const pl: Record<keyof typeof en, string> = {
  'settings.language': 'Język',
  'settings.english': 'English',
  'settings.polish': 'Polski',
  'startup.failed': 'WAVE: nie udało się uruchomić UI\n{error}',
  ...a, ...b, ...c, ...d,
};
