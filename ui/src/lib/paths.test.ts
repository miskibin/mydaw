import { describe, expect, it } from 'vitest';
import { samePath } from './paths';

describe('samePath', () => {
  it('ignores case and separator spelling in Windows paths', () => {
    expect(samePath('C:\\Music\\Song.wave', 'c:\\music\\SONG.WAVE')).toBe(true);
    expect(samePath('C:\\Music\\Song.wave', 'C:/Music/Song.wave')).toBe(true);
    expect(samePath('C:\\Music\\Nowy utwór.wave', 'C:\\MUSIC\\NOWY UTWÓR.WAVE')).toBe(true);
    expect(samePath('\\\\nas\\Share\\Song.wave', '\\\\NAS\\share\\song.wave')).toBe(true);
  });
  it('tells different Windows files apart', () => {
    expect(samePath('C:\\Music\\Song.wave', 'D:\\Music\\Song.wave')).toBe(false);
    expect(samePath('C:\\Music\\Song.wave', 'C:\\Music\\Song 2.wave')).toBe(false);
  });
  it('compares POSIX paths exactly', () => {
    expect(samePath('/home/a/Song.wave', '/home/a/Song.wave')).toBe(true);
    expect(samePath('/home/a/Song.wave', '/home/a/song.wave')).toBe(false);
  });
  it('treats a missing path as different', () => {
    expect(samePath(null, null)).toBe(false);
    expect(samePath('C:\\Song.wave', undefined)).toBe(false);
  });
});
