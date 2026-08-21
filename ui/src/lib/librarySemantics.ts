/**
 * Fast, conservative semantic hints for sample-library rows.
 *
 * This is presentation metadata, never playback truth. Sample-pack names are
 * inconsistent, so the classifier prefers a generic icon over a confident lie.
 * Structural information (especially a loop) is kept separate from sound
 * content: `kick_loop.wav` is both a kick and a loop, with the loop taking the
 * primary icon because it changes how a producer expects to use the file.
 */

export type LibraryEntryKind = 'dir' | 'file';
export type LibraryShape = 'loop' | 'one-shot' | 'stem' | 'songstarter';
export type LibraryContent =
  | 'drum'
  | 'kick'
  | 'snare'
  | 'clap'
  | 'rim'
  | 'hi-hat'
  | 'cymbal'
  | 'tom'
  | 'percussion'
  | 'shaker'
  | '808'
  | 'fill'
  | 'bass'
  | 'synth'
  | 'lead'
  | 'pad'
  | 'pluck'
  | 'keys'
  | 'organ'
  | 'guitar'
  | 'strings'
  | 'brass'
  | 'woodwind'
  | 'bell'
  | 'vocal'
  | 'choir'
  | 'spoken'
  | 'fx'
  | 'impact'
  | 'riser'
  | 'faller'
  | 'transition'
  | 'ambience'
  | 'melody'
  | 'chord'
  | 'arp';

export type LibrarySemanticPrimary = LibraryShape | LibraryContent | 'audio';
export type LibrarySemanticEvidence = 'name' | 'folder' | 'extension';

export interface LibrarySemantic {
  primary: LibrarySemanticPrimary;
  shape: LibraryShape | null;
  content: LibraryContent | null;
  format: 'wav' | 'flac' | 'ogg' | 'mp3' | null;
  evidence: LibrarySemanticEvidence;
  /** Human-readable, combined description such as `Kick loop`. */
  label: string;
}

export interface LibrarySemanticInput {
  name: string;
  relPath: string;
  kind: LibraryEntryKind;
}

interface ContentRule {
  content: LibraryContent;
  match: RegExp;
  negated?: RegExp;
}

/** Regexes are module constants: no expression is compiled while rows render. */
const LOOP = /\b(?:loop|loops|looped|groove|grooves)\b/;
const ONE_SHOT = /\b(?:one shot|one shots|oneshot|oneshots|1 shot|1 shots)\b/;
const STEM = /\b(?:stem|stems|trackout|trackouts|multitrack|multitracks)\b/;
const SONGSTARTER =
  /\b(?:songstarter|songstarters|song starter|song starters|construction kit|construction kits)\b/;

/**
 * Parent folders only supply a shape when the whole folder looks categorical.
 * This prevents a vendor folder such as "Loopmasters House Collection" from
 * turning every nested one-shot into a loop.
 */
const LOOP_FOLDER = /^(?:(?:audio|bass|drum|drums|guitar|melodic|music|perc|percussion|synth|vocal|vox) )?(?:loop|loops|groove|grooves)$/;
const ONE_SHOT_FOLDER = /^(?:(?:audio|drum|drums|sample|samples) )?(?:one shot|one shots|oneshot|oneshots|hits)$/;
const STEM_FOLDER = /^(?:audio )?(?:stem|stems|trackout|trackouts|multitrack|multitracks)$/;
const SONGSTARTER_FOLDER = /^(?:songstarter|songstarters|song starter|song starters|construction kit|construction kits)$/;

const CONTENT_RULES: readonly ContentRule[] = [
  {
    content: 'kick',
    match: /\b(?:kick|kicks|bass drum|bass drums|bassdrum|bassdrums)\b/,
    negated: /\b(?:(?:no|without) (?:kick|kicks|bass drum|bass drums)|kickless)\b/,
  },
  {
    content: 'snare',
    match: /\b(?:snare|snares|snr)\b/,
    negated: /\b(?:(?:no|without) (?:snare|snares)|snareless)\b/,
  },
  { content: 'clap', match: /\b(?:clap|claps|hand clap|hand claps)\b/ },
  { content: 'rim', match: /\b(?:rim|rims|rimshot|rimshots|side stick|sidestick)\b/ },
  { content: 'hi-hat', match: /\b(?:hi hat|hi hats|hihat|hihats|hat|hats)\b/ },
  { content: 'cymbal', match: /\b(?:cymbal|cymbals|crash|crashes|ride|rides|splash|china)\b/ },
  { content: 'tom', match: /\b(?:tom|toms|floor tom|rack tom)\b/ },
  { content: 'shaker', match: /\b(?:shaker|shakers|maraca|maracas|tambourine)\b/ },
  { content: '808', match: /\b(?:808|sub bass|subbass)\b/ },
  {
    content: 'fill',
    match: /\b(?:drum fill|drum fills|fill|fills|breakbeat|breakbeats|drum break|drum breaks|tops)\b/,
  },
  {
    content: 'percussion',
    match: /\b(?:percussion|percussions|perc|conga|congas|bongo|bongos|cowbell|cabasa|clave|claves|guiro)\b/,
  },
  { content: 'drum', match: /\b(?:drum|drums|beat|beats|rhythm|rhythms)\b/ },
  { content: 'bass', match: /\b(?:bass|basses|reese|wobble)\b/ },
  { content: 'lead', match: /\b(?:lead|leads)\b/ },
  { content: 'pad', match: /\b(?:pad|pads)\b/ },
  { content: 'pluck', match: /\b(?:pluck|plucks|plucked)\b/ },
  { content: 'organ', match: /\b(?:organ|organs)\b/ },
  { content: 'keys', match: /\b(?:key|keys|piano|pianos|keyboard|keyboards|rhodes|wurlitzer)\b/ },
  { content: 'guitar', match: /\b(?:guitar|guitars|gtr)\b/ },
  { content: 'strings', match: /\b(?:string|strings|violin|viola|cello|orchestra|orchestral)\b/ },
  { content: 'brass', match: /\b(?:brass|trumpet|trombone|horns)\b/ },
  { content: 'woodwind', match: /\b(?:woodwind|woodwinds|flute|flutes|sax|saxophone|clarinet|oboe)\b/ },
  { content: 'bell', match: /\b(?:bell|bells|mallet|mallets|marimba|vibraphone|glockenspiel|kalimba)\b/ },
  // A playing pattern is more actionable in a browser than the instrument
  // that produced it: "synth arp" should read as an arp, not generic synth.
  { content: 'arp', match: /\b(?:arp|arps|arpeggio|arpeggios)\b/ },
  { content: 'synth', match: /\b(?:synth|synths|synthesizer|synthesizers)\b/ },
  { content: 'choir', match: /\b(?:choir|choirs|chant|chants|choral)\b/ },
  { content: 'spoken', match: /\b(?:spoken|speech|talk|talking|dialogue|dialog)\b/ },
  { content: 'vocal', match: /\b(?:vocal|vocals|vox|voice|voices|acapella|acapellas|a cappella)\b/ },
  { content: 'impact', match: /\b(?:impact|impacts|hit fx|slam|slams|boom|booms)\b/ },
  { content: 'riser', match: /\b(?:riser|risers|uplifter|uplifters|up sweep|upsweep|rise|rises)\b/ },
  { content: 'faller', match: /\b(?:faller|fallers|downlifter|downlifters|down sweep|downsweep|drop fx)\b/ },
  { content: 'transition', match: /\b(?:transition|transitions|sweep|sweeps|swoosh|whoosh|reverse fx)\b/ },
  {
    content: 'ambience',
    match: /\b(?:ambience|ambiences|ambient|atmosphere|atmospheres|atmo|texture|textures|foley|field recording|noise)\b/,
  },
  { content: 'fx', match: /\b(?:fx|sfx|effect|effects)\b/ },
  { content: 'chord', match: /\b(?:chord|chords|chordal)\b/ },
  { content: 'melody', match: /\b(?:melody|melodies|melodic|hook|hooks|motif|motifs)\b/ },
] as const;

const CONTENT_LABELS: Readonly<Record<LibraryContent, string>> = {
  drum: 'Drum',
  kick: 'Kick',
  snare: 'Snare',
  clap: 'Clap',
  rim: 'Rim',
  'hi-hat': 'Hi-hat',
  cymbal: 'Cymbal',
  tom: 'Tom',
  percussion: 'Percussion',
  shaker: 'Shaker',
  '808': '808 / sub',
  fill: 'Drum fill / break',
  bass: 'Bass',
  synth: 'Synth',
  lead: 'Lead',
  pad: 'Pad',
  pluck: 'Pluck',
  keys: 'Keys / piano',
  organ: 'Organ',
  guitar: 'Guitar',
  strings: 'Strings',
  brass: 'Brass',
  woodwind: 'Woodwind',
  bell: 'Bell / mallet',
  vocal: 'Vocal',
  choir: 'Choir / chant',
  spoken: 'Spoken voice',
  fx: 'FX',
  impact: 'Impact',
  riser: 'Riser',
  faller: 'Faller',
  transition: 'Transition / sweep',
  ambience: 'Ambience / texture',
  melody: 'Melody / hook',
  chord: 'Chord',
  arp: 'Arp / arpeggio',
};

const SHAPE_LABELS: Readonly<Record<LibraryShape, string>> = {
  loop: 'Loop',
  'one-shot': 'One-shot',
  stem: 'Stem',
  songstarter: 'Songstarter / construction kit',
};

/** Four thousand entries cover eight maximally-sized directory listings. */
export const LIBRARY_SEMANTIC_CACHE_LIMIT = 4_096;
const semanticCache = new Map<string, LibrarySemantic>();

/**
 * Classify one library entry. Results are cached across tree unmounts and tab
 * switches. The cache deliberately mutates in place: it is an internal LRU,
 * never application state observed by Solid.
 */
export function classifyLibraryEntry(input: LibrarySemanticInput): LibrarySemantic {
  const cacheKey = `${input.kind}\0${input.relPath}\0${input.name}`;
  const cached = semanticCache.get(cacheKey);
  if (cached) {
    semanticCache.delete(cacheKey);
    semanticCache.set(cacheKey, cached);
    return cached;
  }

  const classified = classifyUncached(input);
  semanticCache.set(cacheKey, classified);
  if (semanticCache.size > LIBRARY_SEMANTIC_CACHE_LIMIT) {
    const oldest = semanticCache.keys().next().value as string | undefined;
    if (oldest !== undefined) semanticCache.delete(oldest);
  }
  return classified;
}

/** Test/debug hooks; production code should only call `classifyLibraryEntry`. */
export function clearLibrarySemanticCache(): void {
  semanticCache.clear();
}

export function librarySemanticCacheSize(): number {
  return semanticCache.size;
}

function classifyUncached(input: LibrarySemanticInput): LibrarySemantic {
  const ownName = normalizeWords(stripAudioExtension(input.name));
  const pathSegments = input.relPath
    .split(/[\\/]+/)
    .filter(Boolean)
    .slice(0, -1)
    .map(normalizeWords)
    .filter(Boolean)
    .reverse();

  const ownShape = shapeFromName(ownName);
  const folderShape = pathSegments.map(shapeFromCategoryFolder).find(isPresent) ?? null;
  const shape = ownShape ?? folderShape;
  const ownContent = contentFromWords(ownName);
  const folderContent = pathSegments.map(contentFromWords).find(isPresent) ?? null;
  const content = ownContent ?? folderContent;
  const format = input.kind === 'file' ? audioFormat(input.name) : null;

  const primary = choosePrimary(shape, content);
  const evidence: LibrarySemanticEvidence = ownsPrimary(primary, ownShape, ownContent)
    ? 'name'
    : primary !== 'audio' && (folderShape !== null || folderContent !== null)
      ? 'folder'
      : 'extension';

  return {
    primary,
    shape,
    content,
    format,
    evidence,
    label: semanticLabel(shape, content, format),
  };
}

function choosePrimary(
  shape: LibraryShape | null,
  content: LibraryContent | null,
): LibrarySemanticPrimary {
  if (shape === 'loop' || shape === 'stem' || shape === 'songstarter') return shape;
  if (content !== null) return content;
  return shape ?? 'audio';
}

function ownsPrimary(
  primary: LibrarySemanticPrimary,
  ownShape: LibraryShape | null,
  ownContent: LibraryContent | null,
): boolean {
  return primary === ownShape || primary === ownContent;
}

function semanticLabel(
  shape: LibraryShape | null,
  content: LibraryContent | null,
  format: LibrarySemantic['format'],
): string {
  if (shape === 'loop' && content !== null) return `${CONTENT_LABELS[content]} loop`;
  if (shape === 'one-shot' && content !== null) return `${CONTENT_LABELS[content]} one-shot`;
  if (shape !== null && shape !== 'one-shot') return SHAPE_LABELS[shape];
  if (content !== null) return CONTENT_LABELS[content];
  if (shape !== null) return SHAPE_LABELS[shape];
  return format === null ? 'Audio' : `${format.toUpperCase()} audio`;
}

function shapeFromName(words: string): LibraryShape | null {
  // An explicit loop is stronger than an instrument word or an accidental
  // one-shot marker elsewhere in a vendor filename.
  if (LOOP.test(words)) return 'loop';
  if (STEM.test(words)) return 'stem';
  if (SONGSTARTER.test(words)) return 'songstarter';
  if (ONE_SHOT.test(words)) return 'one-shot';
  return null;
}

function shapeFromCategoryFolder(words: string): LibraryShape | null {
  if (LOOP_FOLDER.test(words)) return 'loop';
  if (STEM_FOLDER.test(words)) return 'stem';
  if (SONGSTARTER_FOLDER.test(words)) return 'songstarter';
  if (ONE_SHOT_FOLDER.test(words)) return 'one-shot';
  return null;
}

function contentFromWords(words: string): LibraryContent | null {
  for (const rule of CONTENT_RULES) {
    if (!rule.match.test(words)) continue;
    if (rule.negated?.test(words)) continue;
    return rule.content;
  }
  return null;
}

function audioFormat(name: string): LibrarySemantic['format'] {
  const match = /\.([^.]+)$/.exec(name.trim());
  const extension = match?.[1]?.toLowerCase();
  return extension === 'wav' || extension === 'flac' || extension === 'ogg' || extension === 'mp3'
    ? extension
    : null;
}

function stripAudioExtension(name: string): string {
  return name.replace(/\.(?:wav|flac|ogg|mp3)$/i, '');
}

function normalizeWords(value: string): string {
  return value
    .normalize('NFKD')
    .replace(/[\u0300-\u036f]/g, '')
    .replace(/([a-z\d])([A-Z])/g, '$1 $2')
    .replace(/([A-Za-z])(\d)/g, '$1 $2')
    .replace(/(\d)([A-Za-z])/g, '$1 $2')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, ' ')
    .trim()
    .replace(/\s+/g, ' ');
}

function isPresent<T>(value: T | null): value is T {
  return value !== null;
}
