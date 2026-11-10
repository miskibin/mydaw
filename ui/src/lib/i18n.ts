import { createSignal } from 'solid-js';
import { en } from '../i18n/en';
import { pl } from '../i18n/pl';
import { names as fxNames } from '../i18n/names-fx';
import { names as synthNames } from '../i18n/names-synth';

export type Lang = 'en' | 'pl';
export type TranslationKey = keyof typeof en;

const [lang, setLangSignal] = createSignal<Lang>('en');
export { lang };

export function setLang(value: Lang): void {
  setLangSignal(value);
  if (typeof document !== 'undefined') document.documentElement.lang = value;
}

export function t(key: TranslationKey, vars: Record<string, string | number> = {}): string {
  const text: string = (lang() === 'pl' ? pl : en)[key];
  return text.replace(/\{([^{}]+)\}/g, (match, name: string) =>
    Object.prototype.hasOwnProperty.call(vars, name) ? String(vars[name]) : match);
}

// Engine-owned names are translated for display only; unknown and user-given names pass through.
const names: Record<string, string> = { ...fxNames, ...synthNames };

export function tName(name: string): string {
  return lang() === 'en' && Object.prototype.hasOwnProperty.call(names, name) ? names[name] : name;
}
