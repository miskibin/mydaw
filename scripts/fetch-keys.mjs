// Prepares the WAVE Keys piano for the installer, in app/resources/keys.
// 1. Downloads the original recordings (24-bit, 138 MB) into target/keys-src, each checked
//    against the SHA-256 pinned in assets-manifest.json; files already there are reused.
// 2. Runs `wave-keys-prep`, which cuts them to the 9 s the engine plays and stores 16-bit
//    FLAC (about 23 MB). See docs/keys.md.
//   node scripts/fetch-keys.mjs
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, readdir, readFile, rename, rm, stat, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const manifest = JSON.parse(await readFile(join(root, 'assets-manifest.json'), 'utf8'));
const kit = manifest.instruments.find(k => k.id === 'salamander-piano');
if (!kit) throw new Error('assets-manifest.json has no salamander-piano kit');
const src = join(root, 'target', 'keys-src');
const out = join(root, 'app', 'resources', 'keys');
await mkdir(src, { recursive: true });
await mkdir(out, { recursive: true });

const sha = b => createHash('sha256').update(b).digest('hex');
async function fetchOne(s) {
  const dst = join(src, s.file);
  try {
    if ((await stat(dst)).size === s.bytes && sha(await readFile(dst)) === s.sha256.toLowerCase()) return false;
  } catch { /* not there yet */ }
  for (let attempt = 1; ; attempt++) {
    try {
      const res = await fetch(s.url);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const buf = Buffer.from(await res.arrayBuffer());
      if (sha(buf) !== s.sha256.toLowerCase()) throw new Error('checksum mismatch');
      await writeFile(dst + '.part', buf);
      await rename(dst + '.part', dst);
      return true;
    } catch (e) {
      if (attempt === 4) throw new Error(`${s.file}: ${e.message}`);
      await new Promise(r => setTimeout(r, 2000 * attempt));
    }
  }
}

let got = 0;
const queue = [...kit.samples];
await Promise.all(Array.from({ length: 6 }, async () => {
  for (let s; (s = queue.shift()); ) if (await fetchOne(s)) got++;
}));
console.log(`WAVE Keys: ${kit.samples.length} originals in ${src} (${got} downloaded)`);

// only what the manifest names goes into the installer
const wanted = new Set(kit.samples.map(s => s.file));
for (const f of await readdir(src)) if (!wanted.has(f)) await rm(join(src, f), { force: true });

const prep = spawnSync('cargo', ['run', '--quiet', '-p', 'wave-keys-prep', '--', src, out], { cwd: root, stdio: 'inherit' });
if (prep.error) throw new Error(`could not run cargo: ${prep.error.message}`);
if (prep.status !== 0) throw new Error(`wave-keys-prep failed (exit ${prep.status})`);

// CC-BY asks that changes be indicated
await writeFile(join(out, 'ATTRIBUTION.txt'), `${kit.attribution}
Changes: each recording is cut to 9 seconds with a fade-out and stored as 16-bit FLAC.
`);
console.log(`WAVE Keys: ready in ${out}`);
