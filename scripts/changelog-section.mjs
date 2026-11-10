#!/usr/bin/env node
// Prints the CHANGELOG.md section of a release (default: the version in app/tauri.conf.json).
// The release workflow uses it as the release body, which is also the "notes" of latest.json
// that the in-app updater shows. It fails when the section is missing or empty, so a release
// cannot go out without notes.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

/** The text under `## <version>` up to the next `## ` heading, trimmed; null when absent or empty. */
export function section(md, version) {
  const lines = md.replace(/\r\n/g, '\n').split('\n');
  const head = /^##\s+v?(\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.+-]*)?)(?:\s+[—–-]+\s+\S.*?)?\s*$/;
  const start = lines.findIndex(l => head.exec(l)?.[1] === version);
  if (start < 0) return null;
  const rest = lines.slice(start + 1);
  const end = rest.findIndex(l => /^##\s/.test(l));
  const text = (end < 0 ? rest : rest.slice(0, end)).join('\n').trim();
  return text || null;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const root = new URL('../', import.meta.url);
  const version = process.argv[2] ?? JSON.parse(readFileSync(new URL('app/tauri.conf.json', root), 'utf8')).version;
  const notes = section(readFileSync(new URL('CHANGELOG.md', root), 'utf8'), version);
  if (!notes) {
    console.error(`CHANGELOG.md has no notes under "## ${version}": write them before releasing.`);
    process.exit(1);
  }
  process.stdout.write(notes + '\n');
}
