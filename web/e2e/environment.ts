import { readFileSync } from 'node:fs';
import { expect, type Page } from '@playwright/test';

interface FirstStart {
  url: string;
  password: string;
}

interface Pulled {
  translations: Record<string, Record<string, string>>;
}

const pulled: Pulled = JSON.parse(
  readFileSync(new URL('../generated/dictionaries.json', import.meta.url), 'utf8'),
);

export function english(key: string, params: Record<string, string> = {}): string {
  const text = pulled.translations.en?.[key];
  if (text === undefined) {
    throw new Error(`the English dictionary has no ${key}`);
  }
  return text.replace(/\{(\w+)\}/g, (whole, name: string) => params[name] ?? whole);
}

export function firstStart(theme: string): FirstStart {
  const servers: Record<string, FirstStart> = JSON.parse(process.env.BOSUN_E2E_FIRST_START ?? '{}');
  const server = servers[theme];
  if (server === undefined) {
    throw new Error(`BOSUN_E2E_FIRST_START names no server for the ${theme} theme; hack/e2e.sh starts them`);
  }
  return server;
}

export async function expectTheme(page: Page, theme: string) {
  await expect(page.locator('.radix-themes').first()).toHaveClass(new RegExp(`(^|\\s)${theme}(\\s|$)`));
}
