import { describe, expect, it } from 'vitest';
import { cmpVersion, parseChangelog, parseInline, parseNotes, releaseOf, releasesSince } from './changelog';
// the real file: the release workflow and the What's new dialog both read it
import changelog from '../../../CHANGELOG.md?raw';
import { section } from '../../../scripts/changelog-section.mjs';
import tauriConf from '../../../app/tauri.conf.json?raw';

const MD = `# Changelog

intro text

## 0.9.3 — 2026-11-10

### Added
- **Updates** in the app.

## 0.9.2 - 2026-11-09
- two
  continued

## 0.9.1
## 0.9.0 — 2026-11-01
- old
`;

describe('parseChangelog', () => {
  const all = parseChangelog(MD);
  it('reads versions, dates and notes in file order', () => {
    expect(all.map(r => r.version)).toEqual(['0.9.3', '0.9.2', '0.9.1', '0.9.0']);
    expect(all[0].date).toBe('2026-11-10');
    expect(all[1].date).toBe('2026-11-09'); // plain hyphen too
    expect(all[2].date).toBeNull();
    expect(all[0].notes).toBe('### Added\n- **Updates** in the app.');
    expect(all[2].notes).toBe('');
  });
  it('accepts Windows line ends and a leading v', () => {
    expect(parseChangelog('## v1.0.0 — 2027-01-01\r\n- a\r\n').map(r => [r.version, r.notes])).toEqual([['1.0.0', '- a']]);
  });
  it('the shipped CHANGELOG.md has a note for the version in tauri.conf.json', () => {
    const conf = JSON.parse(tauriConf);
    const r = parseChangelog(changelog).find(x => x.version === conf.version);
    expect(r, `CHANGELOG.md needs a "## ${conf.version}" section`).toBeTruthy();
    expect(r!.notes.length).toBeGreaterThan(10);
  });
});

describe('cmpVersion', () => {
  it('orders numerically, not as text', () => {
    expect(cmpVersion('0.9.10', '0.9.9')).toBe(1);
    expect(cmpVersion('0.10.0', '0.9.99')).toBe(1);
    expect(cmpVersion('1.0.0', '1.0.0')).toBe(0);
    expect(cmpVersion('v0.9.2', '0.9.3')).toBe(-1);
  });
  it('puts a pre-release before its release', () => {
    expect(cmpVersion('1.0.0-beta.1', '1.0.0')).toBe(-1);
    expect(cmpVersion('1.0.0', '1.0.0-rc.1')).toBe(1);
    expect(cmpVersion('1.0.0-alpha', '1.0.0-beta')).toBe(-1);
  });
});

describe('releasesSince', () => {
  const all = parseChangelog(MD);
  it('lists every version after the last seen one, newest first', () => {
    expect(releasesSince(all, '0.9.0', '0.9.3').map(r => r.version)).toEqual(['0.9.3', '0.9.2']); // 0.9.1 has no notes
  });
  it('is empty when nothing is new or the user went back', () => {
    expect(releasesSince(all, '0.9.3', '0.9.3')).toEqual([]);
    expect(releasesSince(all, '0.9.3', '0.9.2')).toEqual([]);
  });
  it('never shows a version newer than the one running', () => {
    expect(releasesSince(all, '0.9.0', '0.9.2').map(r => r.version)).toEqual(['0.9.2']);
  });
  it('finds the notes of the running version, or the newest older one', () => {
    expect(releaseOf(all, '0.9.3')?.version).toBe('0.9.3');
    expect(releaseOf(all, '0.9.1')?.version).toBe('0.9.0'); // 0.9.1 is empty
    expect(releaseOf(all, '0.1.0')).toBeNull();
  });
});

describe('release notes Markdown', () => {
  it('splits bold, code and links without ever keeping the address', () => {
    expect(parseInline('a **b** `c` [d](https://x.y) e')).toEqual([
      { t: 'text', s: 'a ' }, { t: 'b', s: 'b' }, { t: 'text', s: ' ' }, { t: 'code', s: 'c' }, { t: 'text', s: ' ' }, { t: 'text', s: 'd' }, { t: 'text', s: ' e' },
    ]);
  });
  it('reads headings, bullets, wrapped bullets and paragraphs', () => {
    expect(parseNotes('### Added\n- one\n  wrapped\n* two\n\nplain\ntext')).toEqual([
      { t: 'h', s: 'Added' },
      { t: 'li', inl: [{ t: 'text', s: 'one wrapped' }] },
      { t: 'li', inl: [{ t: 'text', s: 'two' }] },
      { t: 'p', inl: [{ t: 'text', s: 'plain text' }] },
    ]);
  });
  it('treats raw HTML as text (rendered with textContent, never as markup)', () => {
    expect(parseNotes('- <img src=x onerror=alert(1)>')[0]).toEqual({ t: 'li', inl: [{ t: 'text', s: '<img src=x onerror=alert(1)>' }] });
  });
  it('is empty for empty notes', () => {
    expect(parseNotes('')).toEqual([]);
  });
});

describe('scripts/changelog-section.mjs (release body)', () => {
  it('returns one version\'s notes, and nothing for a missing or empty version', () => {
    expect(section(MD, '0.9.3')).toBe('### Added\n- **Updates** in the app.');
    expect(section(MD, '0.9.2')).toBe('- two\n  continued');
    expect(section(MD, '0.9.1')).toBeNull(); // empty
    expect(section(MD, '1.0.0')).toBeNull(); // absent
    expect(section(MD, '0.9')).toBeNull();
  });
  it('agrees with the in-app parser on the real file', () => {
    for (const r of parseChangelog(changelog)) expect(section(changelog, r.version) ?? '').toBe(r.notes);
  });
});
