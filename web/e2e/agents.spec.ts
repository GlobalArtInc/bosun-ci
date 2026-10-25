import { expect, type Page, test } from '@playwright/test';
import { agentIn, asOwner, created, ownerHeaders, pool, project, registered, unique } from './bosun';
import { english } from './environment';

interface Agent {
  id: string;
  name: string;
  drain: boolean;
}

interface Agents {
  items: Agent[];
}

const COMMAND = /^curl -fsSL http:\/\/[^ ]+\/install-agent\.sh \| sudo sh -s -- --token (brt_[A-Za-z0-9]+)$/;

test.use({
  extraHTTPHeaders: async ({}, use) => {
    await use(ownerHeaders());
  },
});

function agentRow(page: Page, name: string) {
  return page.getByRole('row').filter({ has: page.getByText(name, { exact: true }) });
}

test('creating a registration token shows the install command once', async ({ page, playwright }, info) => {
  const owner = await asOwner(playwright);
  const path = await project(owner, unique(info.project.name));
  const name = unique(info.project.name);

  await page.goto('/queue');
  await expect(page.getByRole('heading', { name: english('shell.nav.agents'), level: 1 })).toBeVisible();
  const form = page.getByRole('form', { name: english('poolPage.create.title') });
  await form.getByLabel(english('poolPage.create.name'), { exact: true }).fill(name);
  await form.getByLabel(english('poolPage.create.projects'), { exact: true }).fill(path);
  await form.getByRole('button', { name: english('poolPage.create.submit') }).click();
  await expect(page).toHaveURL(new RegExp(`/pools/${name}$`));
  await expect(page.getByRole('heading', { name, level: 1 })).toBeVisible();

  await page.getByRole('button', { name: english('poolPage.token.create') }).click();
  const shown = page.getByLabel(english('poolPage.token.label'));
  await expect(shown).toHaveText(COMMAND);
  const token = COMMAND.exec((await shown.textContent()) ?? '')?.[1] ?? '';

  const joined = unique(info.project.name);
  const agent = await registered(owner, token, joined);
  try {
    const agents = page.getByRole('region', { name: english('queuePage.agents.title') });
    await expect(agentRow(page, joined)).toContainText(english('agentsPage.state.offline'));
    await expect(agents).toContainText(joined);

    await page.reload();
    await expect(page.getByRole('heading', { name, level: 1 })).toBeVisible();
    await expect(agentRow(page, joined)).toBeVisible();
    await expect(page.getByLabel(english('poolPage.token.label'))).toHaveCount(0);
    await expect(page.locator('body')).not.toContainText(token);
  } finally {
    expect((await owner.delete(`/api/v1/agents/${agent}`)).status()).toBe(204);
  }
});

test('draining an agent shows it as draining', async ({ page, playwright }, info) => {
  const owner = await asOwner(playwright);
  const path = await project(owner, unique(info.project.name));
  const chosen = await pool(owner, unique(info.project.name), path);
  const name = unique(info.project.name);
  const agent = await agentIn(owner, chosen, name);

  await page.goto('/queue');
  const listed = page
    .getByRole('region', { name: english('queuePage.agents.title') })
    .getByRole('region', { name: chosen });
  const row = listed.getByRole('row').filter({ has: page.getByText(name, { exact: true }) });
  await expect(row).toContainText(english('agentsPage.state.offline'));
  await expect(row).not.toContainText(english('agentsPage.state.draining'));

  await row.getByRole('button', { name: english('agentsPage.drain'), exact: true }).click();
  await expect(row).toContainText(english('agentsPage.state.draining'));
  await expect(row.getByRole('button', { name: english('agentsPage.lift'), exact: true })).toBeVisible();
  const drained = await created<Agents>(owner.get('/api/v1/agents?limit=500'));
  expect(drained.items.find((item) => item.id === agent)?.drain).toBe(true);

  await page.reload();
  await expect(row).toContainText(english('agentsPage.state.draining'));

  await row.getByRole('button', { name: english('agentsPage.delete'), exact: true }).click();
  await page
    .getByRole('alertdialog')
    .getByRole('button', { name: english('agentsPage.delete'), exact: true })
    .click();
  await expect(row).toHaveCount(0);
  await expect(listed).toContainText(english('agentsPage.empty'));
  const left = await created<Agents>(owner.get('/api/v1/agents?limit=500'));
  expect(left.items.some((item) => item.id === agent)).toBe(false);
});
