import { afterEach, describe, expect, it, vi } from 'vitest';
import { en } from '../i18n/en';
import { pl } from '../i18n/pl';
import { initPrefs, prefs } from '../state/prefs';
import { lang, setLang, t, tName } from './i18n';

vi.mock('../state/actions', () => ({ projectSave: vi.fn() }));

afterEach(() => { setLang('en'); vi.unstubAllGlobals(); });

describe('localization', () => {
  it('has matching dictionary keys', () => {
    expect(Object.keys(pl).sort()).toEqual(Object.keys(en).sort());
  });
  it('defaults to English and interpolates values', () => {
    expect(lang()).toBe('en');
    expect(t('startup.failed', { error: 'test' })).toBe('WAVE: failed to start UI\ntest');
  });
  it('passes unknown names through', () => {
    expect(tName('User preset')).toBe('User preset');
  });
  it('loads the saved language and updates the document', () => {
    vi.stubGlobal('localStorage', { getItem: () => '{"lang":"pl"}' });
    const documentElement = { lang: '' };
    vi.stubGlobal('document', { documentElement });
    initPrefs();
    expect(prefs().lang).toBe('pl');
    expect(lang()).toBe('pl');
    expect(documentElement.lang).toBe('pl');
  });
});
