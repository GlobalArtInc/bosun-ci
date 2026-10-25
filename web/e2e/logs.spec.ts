import { type APIRequestContext, expect, type Page, test } from '@playwright/test';
import { asOwner, created, dispatch, gate, person, project, signIn, unique, workflow } from './bosun';
import { english } from './environment';

interface Job {
  id: string;
  key: string;
}

interface Jobs {
  items: Job[];
}

const JOB_MS = 60_000;

const ANNOTATED = `name: annotated
on: workflow_dispatch
jobs:
  check:
    runs-on: [self-hosted]
    steps:
      - name: Lint
        run: |
          echo "checking the sources"
          echo "::error file=src/app.ts,line=3,title=lint::the value is never read"
          echo "after the error"
          exit 1
`;

function followed(url: string): string {
  return `name: followed
on: workflow_dispatch
jobs:
  print:
    runs-on: [self-hosted]
    steps:
      - name: Count
        run: |
          echo "the first line"
          until curl -fs --noproxy '*' -o /dev/null "${url}"; do sleep 0.2; done
          echo "the line after the gate"
`;
}

async function jobOf(owner: APIRequestContext, run: string, key: string): Promise<string> {
  const jobs = await created<Jobs>(owner.get(`/api/v1/runs/${run}/attempts/1/jobs`));
  const found = jobs.items.find((item) => item.key === key);
  if (found === undefined) {
    throw new Error(`the run ${run} has no job ${key}`);
  }
  return found.id;
}

function logOf(page: Page, step: string) {
  return page.getByRole('region', { name: english('runPage.log.label').replace('{name}', step) });
}

test('an annotation of a failing step appears at its line', async ({ page, playwright }, info) => {
  const owner = await asOwner(playwright);
  const path = await project(owner, unique(info.project.name));
  const annotated = await workflow(owner, path, ANNOTATED);
  const viewer = await person(owner, unique(info.project.name), 'viewer', path);
  await signIn(page, viewer);

  const run = await dispatch(owner, annotated);
  const job = await jobOf(owner, run, 'check');
  await page.goto(`/runs/${run}?job=${job}`);

  const list = page.getByRole('list', { name: english('runPage.annotations.title') });
  await expect(list).toContainText('the value is never read', { timeout: JOB_MS });
  await expect(list).toContainText('src/app.ts:3');
  await list.getByRole('link', { name: english('runPage.annotations.show'), exact: true }).click();

  const line = logOf(page, 'Lint').locator('[aria-current="location"]');
  await expect(line).toContainText('the value is never read');
  await expect(line).toContainText('lint · src/app.ts:3');
  await expect(line).toHaveAttribute('data-annotation', 'error');
  await expect(logOf(page, 'Lint')).toContainText('checking the sources');
  await expect(logOf(page, 'Lint')).toContainText('after the error');
});

test('a followed step appends its lines live', async ({ page, playwright }, info) => {
  const opening = await gate();
  try {
    const owner = await asOwner(playwright);
    const path = await project(owner, unique(info.project.name));
    const counting = await workflow(owner, path, followed(opening.url));
    const viewer = await person(owner, unique(info.project.name), 'viewer', path);
    await signIn(page, viewer);

    const run = await dispatch(owner, counting);
    const job = await jobOf(owner, run, 'print');
    await page.goto(`/runs/${run}?job=${job}`);
    await page.evaluate(() => {
      document.documentElement.dataset.e2eLoaded = 'once';
    });
    await page.getByRole('button', { name: 'Count', exact: true }).click({ timeout: JOB_MS });

    const log = logOf(page, 'Count');
    await expect(log).toContainText('the first line', { timeout: JOB_MS });
    await expect(page.getByText(english('runPage.log.following'))).toBeVisible();
    await expect(log).not.toContainText('the line after the gate');

    opening.open();
    await expect(log).toContainText('the line after the gate', { timeout: JOB_MS });
    await expect(page.getByText(english('runPage.log.following'))).toHaveCount(0, { timeout: JOB_MS });
    expect(await page.evaluate(() => document.documentElement.dataset.e2eLoaded)).toBe('once');
  } finally {
    await opening.close();
  }
});
