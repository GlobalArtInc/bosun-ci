import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const BUNDLES =
  'https://api.ownlate.com/public/v1/ota/b3f75001bc7444b79c472ebead725429d81ba42a8da24badb4a4e1e18be33914/bundles';

const LANGUAGES = ['en', 'ru'];

const TIMEOUT_MS = 30_000;

const TARGET = join(dirname(fileURLToPath(import.meta.url)), '..', 'generated', 'dictionaries.json');

function fail(message) {
  console.error(`dictionaries: ${message}`);
  process.exit(1);
}

function pick(translations, language) {
  const code = Object.keys(translations).find((name) => name.toLowerCase().split(/[-_]/)[0] === language);
  return code === undefined ? undefined : translations[code];
}

let response;
try {
  response = await fetch(BUNDLES, { signal: AbortSignal.timeout(TIMEOUT_MS) });
} catch (error) {
  fail(`OwnLate could not be reached: ${error instanceof Error ? error.message : String(error)}`);
}
if (!response.ok) {
  fail(`OwnLate answered ${response.status} ${response.statusText} for the bundles of the interface`);
}
const bundle = await response.json();
if (typeof bundle?.translations !== 'object' || bundle.translations === null) {
  fail('the bundle of the interface holds no translations');
}
const translations = {};
for (const language of LANGUAGES) {
  const dictionary = pick(bundle.translations, language);
  if (typeof dictionary !== 'object' || dictionary === null) {
    fail(`the bundle of the interface has no dictionary for ${language}`);
  }
  translations[language] = Object.fromEntries(
    Object.entries(dictionary)
      .filter(([, text]) => typeof text === 'string')
      .sort(([left], [right]) => left.localeCompare(right)),
  );
}
await mkdir(dirname(TARGET), { recursive: true });
await writeFile(TARGET, `${JSON.stringify({ version: bundle.version ?? null, translations }, null, 2)}\n`);
const counts = LANGUAGES.map((language) => `${Object.keys(translations[language]).length} ${language}`).join(
  ', ',
);
console.log(`dictionaries: pulled release ${bundle.version} of the interface's OwnLate project: ${counts}`);
