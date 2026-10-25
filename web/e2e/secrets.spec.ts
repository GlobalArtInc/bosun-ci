import { randomBytes } from 'node:crypto';
import { expect, test } from '@playwright/test';
import { asOwner, created, person, project, signIn, unique } from './bosun';
import { english } from './environment';

interface Secret {
  name: string;
  value?: string;
}

interface Secrets {
  items: Secret[];
}

interface Me {
  csrf_token: string;
}

interface Refused {
  code: string;
}

interface Refusal {
  error: Refused;
}

interface Answer {
  status: number;
  code: string;
}

interface Attempt {
  scope: string;
}

test('after setting a secret, only its name and date are shown', async ({ page, playwright }, info) => {
  const owner = await asOwner(playwright);
  const path = await project(owner, unique(info.project.name));
  const maintainer = await person(owner, unique(info.project.name), 'maintainer', path);
  const value = `a value nobody reads back ${randomBytes(8).toString('hex')}`;
  await signIn(page, maintainer);

  await page.goto(`/projects/${path}/settings`);
  const secrets = page.getByRole('region', { name: english('settingsPage.secrets.title') });
  await expect(secrets).toContainText(english('settingsPage.secrets.empty'));
  const form = secrets.getByRole('form', { name: english('settingsPage.secrets.setTitle') });
  await form.getByLabel(english('settingsPage.column.name'), { exact: true }).fill('deploy_key');
  await form.getByLabel(english('settingsPage.column.value'), { exact: true }).fill(value);
  await form.getByRole('button', { name: english('settingsPage.secrets.set'), exact: true }).click();

  await expect(secrets.getByRole('status')).toHaveText(
    english('settingsPage.secrets.saved').replace('{name}', 'DEPLOY_KEY'),
  );
  const row = secrets.getByRole('row').filter({ has: page.getByText('DEPLOY_KEY', { exact: true }) });
  await expect(row).toBeVisible();
  const year = String(new Date().getFullYear());
  await expect(row.locator('[title]')).toHaveCount(2);
  for (const title of await row.locator('[title]').all()) {
    await expect(title).toHaveAttribute('title', new RegExp(year));
  }
  await expect(
    secrets
      .getByRole('form', { name: english('settingsPage.secrets.setTitle') })
      .getByLabel(english('settingsPage.column.value'), { exact: true }),
  ).toHaveValue('');
  await expect(page.locator('body')).not.toContainText(value);

  await page.reload();
  await expect(row).toBeVisible();
  await expect(page.locator('body')).not.toContainText(value);
  const listed = await created<Secrets>(owner.get(`/api/v1/secrets?project=${path}`));
  expect(listed.items.map((item) => [item.name, item.value])).toEqual([['DEPLOY_KEY', undefined]]);
});

test('a developer sees no control to write a secret, and the API refuses the write anyway', async ({
  page,
  playwright,
}, info) => {
  const owner = await asOwner(playwright);
  const path = await project(owner, unique(info.project.name));
  await created<Secret>(
    owner.post(`/api/v1/secrets?project=${path}`, { data: { name: 'RELEASE_TOKEN', value: 'kept' } }),
  );
  const developer = await person(owner, unique(info.project.name), 'developer', path);
  await signIn(page, developer);

  await page.goto(`/projects/${path}/settings`);
  const secrets = page.getByRole('region', { name: english('settingsPage.secrets.title') });
  await expect(secrets.getByText('RELEASE_TOKEN', { exact: true })).toBeVisible();
  await expect(secrets.getByRole('form')).toHaveCount(0);
  await expect(secrets.getByRole('button')).toHaveCount(0);
  await expect(secrets.getByRole('textbox')).toHaveCount(0);

  const answers = await page.evaluate(
    async ({ scope }: Attempt) => {
      const me: Me = await (await fetch('/api/v1/me')).json();
      const headers = { 'Content-Type': 'application/json', 'X-Bosun-CSRF': me.csrf_token };
      const attempts = [
        fetch(`/api/v1/secrets?project=${scope}`, {
          method: 'POST',
          headers,
          body: JSON.stringify({ name: 'RELEASE_TOKEN', value: 'replaced' }),
        }),
        fetch(`/api/v1/secrets/RELEASE_TOKEN?project=${scope}`, { method: 'DELETE', headers }),
      ];
      const answered: Answer[] = [];
      for (const attempt of attempts) {
        const response = await attempt;
        const refusal: Refusal = await response.json();
        answered.push({ status: response.status, code: refusal.error.code });
      }
      return answered;
    },
    { scope: path },
  );
  expect(answers).toEqual([
    { status: 403, code: 'forbidden' },
    { status: 403, code: 'forbidden' },
  ]);
  const kept = await created<Secrets>(owner.get(`/api/v1/secrets?project=${path}`));
  expect(kept.items.map((item) => item.name)).toEqual(['RELEASE_TOKEN']);
});
