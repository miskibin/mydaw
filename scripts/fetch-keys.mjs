// Downloads the WAVE Keys piano into app/resources/keys (bundled into the installer).
// Every file is checked against the SHA-256 pinned in assets-manifest.json.
//   node scripts/fetch-keys.mjs
import { createHash } from 'node:crypto';
import { mkdir, readFile, rename, stat, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const manifest = JSON.parse(await readFile(join(root, 'assets-manifest.json'), 'utf8'));
const kit = manifest.instruments.find(k => k.id === 'salamander-piano');
if (!kit) throw new Error('assets-manifest.json has no salamander-piano kit');
const out = join(root, 'app', 'resources', 'keys');
await mkdir(out, { recursive: true });

const sha = b => createHash('sha256').update(b).digest('hex');
async function fetchOne(s) {
  const dst = join(out, s.file);
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
await writeFile(join(out, 'ATTRIBUTION.txt'), kit.attribution + '\n');
console.log(`WAVE Keys: ${kit.samples.length} files in ${out} (${got} downloaded)`);
