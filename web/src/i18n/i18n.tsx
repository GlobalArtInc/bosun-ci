import { createContext, type ReactNode, useCallback, useContext, useEffect, useMemo, useState } from 'react';
import pulled from '../../generated/dictionaries.json';

export const LANGUAGES = ['en', 'ru'] as const;

type Language = (typeof LANGUAGES)[number];

export type Key = keyof typeof pulled.translations.en;

type Params = Record<string, string | number>;

export type Dictionary = Record<string, string>;

export const dictionaries: Record<Language, Dictionary> = pulled.translations;

export const LANGUAGE_NAMES: Record<Language, string> = { en: 'English', ru: 'Русский' };

const STORAGE_KEY = 'bosun.language';

const PLACEHOLDER = /\{(\w+)\}/g;

export interface I18n {
  language: Language;
  setLanguage: (language: Language) => void;
  t: (key: Key, params?: Params) => string;
}

interface I18nProviderProps {
  children: ReactNode;
}

export function isLanguage(value: unknown): value is Language {
  return typeof value === 'string' && (LANGUAGES as readonly string[]).includes(value);
}

export function translate(language: Language, key: Key, params: Params = {}): string {
  const text = dictionaries[language][key] ?? dictionaries.en[key] ?? key;
  return text.replace(PLACEHOLDER, (whole, name: string) => (name in params ? String(params[name]) : whole));
}

function stored(): Language | null {
  try {
    const value = window.localStorage.getItem(STORAGE_KEY);
    return isLanguage(value) ? value : null;
  } catch {
    return null;
  }
}

function preferred(): Language {
  for (const tag of navigator.languages ?? [navigator.language]) {
    const primary = tag.toLowerCase().split('-')[0];
    if (isLanguage(primary)) {
      return primary;
    }
  }
  return 'en';
}

const I18nContext = createContext<I18n | null>(null);

export function I18nProvider({ children }: I18nProviderProps) {
  const [language, setState] = useState<Language>(() => stored() ?? preferred());

  useEffect(() => {
    document.documentElement.lang = language;
  }, [language]);

  const setLanguage = useCallback((next: Language) => {
    setState(next);
    try {
      window.localStorage.setItem(STORAGE_KEY, next);
    } catch {
      return;
    }
  }, []);

  const value = useMemo<I18n>(
    () => ({ language, setLanguage, t: (key, params) => translate(language, key, params) }),
    [language, setLanguage],
  );

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18n {
  const value = useContext(I18nContext);
  if (value === null) {
    throw new Error('useI18n is used outside I18nProvider');
  }
  return value;
}
