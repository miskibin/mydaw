import { en as a } from './part-a';
import { en as b } from './part-b';
import { en as c } from './part-c';
import { en as d } from './part-d';

const base = {
  'settings.language': 'Language',
  'settings.english': 'English',
  'settings.polish': 'Polski',
  'startup.failed': 'WAVE: failed to start UI\n{error}',
} as const;

export const en = { ...base, ...a, ...b, ...c, ...d } as const;
