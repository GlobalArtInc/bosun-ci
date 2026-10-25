import { describe, expect, it } from 'vitest';
import { type Dictionary, dictionaries, LANGUAGES, translate } from './i18n';

const sources = import.meta.glob<string>(['../**/*.{ts,tsx}', '!../**/*.test.{ts,tsx}', '!../test/**'], {
  query: '?raw',
  import: 'default',
  eager: true,
});

const USE = /\bt\(\s*'([^']+)'/g;

const QUOTED = /'([A-Za-z][A-Za-z0-9]*(?:\.[A-Za-z0-9_]+)+)'/g;

const PLACEHOLDER = /\{(\w+)\}/g;

function usedKeys(): Map<string, string[]> {
  const used = new Map<string, string[]>();
  const note = (key: string, file: string) => {
    const files = used.get(key) ?? [];
    if (!files.includes(file)) {
      used.set(key, [...files, file]);
    }
  };
  for (const [file, text] of Object.entries(sources)) {
    for (const match of text.matchAll(USE)) {
      note(match[1] ?? '', file);
    }
  }
  const namespaces = new Set([...used.keys()].map((key) => key.split('.')[0]));
  for (const [file, text] of Object.entries(sources)) {
    for (const match of text.matchAll(QUOTED)) {
      const key = match[1] ?? '';
      if (namespaces.has(key.split('.')[0])) {
        note(key, file);
      }
    }
  }
  return used;
}

function placeholders(text: string | undefined): string {
  return [...(text ?? '').matchAll(PLACEHOLDER)]
    .map((match) => match[1])
    .sort()
    .join(',');
}

function missing(used: Map<string, string[]>, available: Record<string, Dictionary>): string[] {
  const problems: string[] = [];
  for (const [key, files] of used) {
    for (const [language, dictionary] of Object.entries(available)) {
      if (!(key in dictionary)) {
        problems.push(`${language} lacks ${key}, used in ${files.join(', ')}`);
      }
    }
  }
  return problems;
}

describe('the dictionaries pulled from OwnLate', () => {
  const used = usedKeys();

  it('find the keys the code uses', () => {
    expect(used.has('signIn.submit')).toBe(true);
    expect(used.get('shell.user.signOut')?.length).toBeGreaterThan(1);
    expect(used.has('password.error.mismatch')).toBe(true);
  });

  it('hold every key the code uses, in every language', () => {
    expect(missing(used, dictionaries)).toEqual([]);
  });

  it('give every used key the same placeholders in every language', () => {
    const problems: string[] = [];
    for (const key of used.keys()) {
      const english = placeholders(dictionaries.en[key]);
      for (const language of LANGUAGES) {
        if (key in dictionaries[language] && placeholders(dictionaries[language][key]) !== english) {
          problems.push(`${language} ${key}`);
        }
      }
    }
    expect(problems).toEqual([]);
  });

  it('report a key that a dictionary lacks', () => {
    const used = new Map([
      ['signIn.submit', ['pages/sign-in.tsx']],
      ['signIn.nowhere', ['pages/sign-in.tsx']],
    ]);
    expect(missing(used, { en: { 'signIn.submit': 'Sign in' }, ru: {} })).toEqual([
      'ru lacks signIn.submit, used in pages/sign-in.tsx',
      'en lacks signIn.nowhere, used in pages/sign-in.tsx',
      'ru lacks signIn.nowhere, used in pages/sign-in.tsx',
    ]);
  });

  it('fill the placeholders of a text', () => {
    expect(translate('en', 'shell.user.signedInAs', { login: 'admin' })).toContain('admin');
    expect(translate('ru', 'shell.user.signedInAs', { login: 'admin' })).toContain('admin');
  });
});
