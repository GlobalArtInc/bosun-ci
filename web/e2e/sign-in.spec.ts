import AxeBuilder from '@axe-core/playwright';
import { expect, type Page, test } from '@playwright/test';
import { english, expectTheme, firstStart } from './environment';

const NEW_PASSWORD = 'a longer passphrase of my own';

const TAB_STOPS = 20;

interface Stop {
  name: string;
  indicated: boolean;
}

async function signIn(page: Page, password: string) {
  await page.getByLabel(english('signIn.login'), { exact: true }).fill('admin');
  await page.getByLabel(english('signIn.password'), { exact: true }).fill(password);
  await page.getByRole('button', { name: english('signIn.submit'), exact: true }).click();
}

async function focused(page: Page): Promise<Stop | null> {
  return page.evaluate(() => {
    const element = document.activeElement;
    if (element === null || element === document.body) {
      return null;
    }
    const ring = element.closest('.rt-TextFieldRoot') ?? element;
    const style = getComputedStyle(ring);
    const outlined = style.outlineStyle !== 'none' && Number.parseFloat(style.outlineWidth) > 0;
    const name =
      element.getAttribute('aria-label') ??
      element.getAttribute('name') ??
      element.textContent?.trim() ??
      element.tagName;
    return { name, indicated: outlined || style.boxShadow !== 'none' };
  });
}

test('admin signs in with admin.password, sets a new password, reaches home and signs out', async ({
  page,
}, info) => {
  const server = firstStart(info.project.name);

  await page.goto(`${server.url}/`);
  await expect(page).toHaveURL(`${server.url}/sign-in`);
  await expectTheme(page, info.project.name);
  await signIn(page, server.password);

  await expect(page).toHaveURL(`${server.url}/password`);
  await expect(page.getByRole('heading', { name: english('password.title') })).toBeVisible();
  await page.getByLabel(english('password.current'), { exact: true }).fill(server.password);
  await page.getByLabel(english('password.new'), { exact: true }).fill(NEW_PASSWORD);
  await page.getByLabel(english('password.repeat'), { exact: true }).fill(NEW_PASSWORD);
  await page.getByRole('button', { name: english('password.submit'), exact: true }).click();

  await expect(page).toHaveURL(`${server.url}/`);
  await expect(page.getByRole('heading', { name: english('home.title') })).toBeVisible();
  await expectTheme(page, info.project.name);
  await page.getByRole('button', { name: 'admin', exact: true }).click();
  await page.getByRole('menuitem', { name: english('shell.user.signOut') }).click();

  await expect(page).toHaveURL(`${server.url}/sign-in`);
  expect((await page.request.get(`${server.url}/api/v1/me`)).status()).toBe(401);
  await signIn(page, NEW_PASSWORD);
  await expect(page).toHaveURL(`${server.url}/`);
  await expect(page.getByRole('heading', { name: english('home.title') })).toBeVisible();
});

test('the sign-in page has no contrast or focus violation', async ({ page }, info) => {
  await page.goto('/sign-in');
  await expect(page.getByRole('button', { name: english('signIn.submit'), exact: true })).toBeVisible();
  await expectTheme(page, info.project.name);

  const audit = await new AxeBuilder({ page })
    .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
    .analyze();
  expect(audit.violations.map((violation) => `${violation.id}: ${violation.help}`)).toEqual([]);

  const first = await focused(page);
  expect(first?.name).toBe('login');
  const stops: Stop[] = first === null ? [] : [first];
  for (let index = 0; index < TAB_STOPS; index += 1) {
    await page.keyboard.press('Tab');
    const stop = await focused(page);
    if (stop === null) {
      continue;
    }
    if (stops.some((seen) => seen.name === stop.name)) {
      break;
    }
    stops.push(stop);
  }
  expect(stops.filter((stop) => !stop.indicated)).toEqual([]);
  const toggle =
    info.project.name === 'dark' ? english('shell.theme.toLight') : english('shell.theme.toDark');
  expect(stops.map((stop) => stop.name)).toEqual(
    expect.arrayContaining([
      'Bosun',
      english('shell.language.label'),
      toggle,
      'login',
      'password',
      english('signIn.submit'),
    ]),
  );
  const names = stops.map((stop) => stop.name);
  expect(names.indexOf('login')).toBeLessThan(names.indexOf('password'));
  expect(names.indexOf('password')).toBeLessThan(names.indexOf(english('signIn.submit')));
});
