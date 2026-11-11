// The standard library and the projects the program starts with, as the engine defines them
// (crates/engine/src/factory.rs, demo.rs). `api/factory.json` is generated from the engine and a
// test there fails when it is out of date, so the in-browser mock cannot drift from it.
import data from '../api/factory.json';
import { SAMPLE_ROOT, samplerDefault, VOL_UNITY, type Channel, type Id, type Project, type Source } from '../api/types';

export interface FactoryFile { file: string; name: string; group: string; root: number; dur: number; ch: number; bytes: number; color: string }
export const FACTORY_KIT: string = data.kit;
export const FACTORY_FILES: readonly FactoryFile[] = data.files;
export const LIB_SCHEME = 'lib:';
export const libPath = (file: string) => `${LIB_SCHEME}${FACTORY_KIT}/${file}`;
/** The standard-library file a stored path names (`lib:Standard/<file>`), if it is one. */
export const factoryFile = (path: string) => FACTORY_FILES.find(f => libPath(f.file) === path);
/** File name → channel name of a new project's Channel Rack (model.rs `STARTER_KIT`). */
export const STARTER_KIT: readonly [string, string][] = [['kick-808-short.flac', 'Kick'], ['clap-808.flac', 'Clap'], ['hat-808-closed.flac', 'Hat'], ['snare-808.flac', 'Snare']];

export const emptyProject = (): Project => structuredClone(data.empty) as unknown as Project;
export const demoProject = (): Project => structuredClone(data.demo) as unknown as Project;

/** The project source of a standard-library file: the one already there, or a new one (factory.rs `source`). */
export function factorySource(p: Project, file: string): Id | null {
  const f = FACTORY_FILES.find(x => x.file === file); if (!f) return null;
  const path = libPath(file), have = p.sources.find(s => s.path === path);
  if (have) return have.id;
  const s: Source = { id: p.nextId++, name: f.name, color: f.color, path, dur: f.dur, sr: 44100, ch: f.ch, peakDb: -1 };
  p.sources.push(s);
  return s.id;
}
/** The sampler channel of a standard-library file: the first there is, or a new one (model.rs `factory_channel`). */
export function factoryChannel(p: Project, file: string): Id | null {
  const f = FACTORY_FILES.find(x => x.file === file), source = factorySource(p, file);
  if (!f || source == null) return null;
  const chans = p.channels ?? (p.channels = []);
  const have = chans.find(c => c.kind === 'sample' && c.source === source);
  if (have) return have.id;
  const c: Channel = { id: p.nextId++, name: f.name, color: f.color, kind: 'sample', source, sampler: samplerDefault(), vol: VOL_UNITY, pan: 0, mute: false, solo: false, insert: null, root: f.root ?? SAMPLE_ROOT };
  chans.push(c);
  return c.id;
}
/** Kick, Clap, Hat, Snare samplers (model.rs `add_starter_kit`); returns their ids. */
export function addStarterKit(p: Project): Id[] {
  return STARTER_KIT.map(([file, name]) => {
    const id = factoryChannel(p, file)!;
    p.channels!.find(c => c.id === id)!.name = name;
    return id;
  });
}
