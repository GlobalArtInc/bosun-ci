import { expect, type Locator, type Page, test } from '@playwright/test';
import {
  asOwner,
  created,
  dispatch,
  gate,
  NO_TARGET,
  person,
  project,
  signIn,
  unique,
  workflow,
} from './bosun';
import { english } from './environment';

interface Job {
  id: string;
  key: string;
}

interface Jobs {
  items: Job[];
}

interface Run {
  status: string;
  conclusion: string | null;
}

interface Refused {
  code: string;
}

interface Refusal {
  error: Refused;
}

interface Me {
  csrf_token: string;
}

interface Answer {
  status: number;
  code: string;
}

interface Painted {
  tone: string | undefined;
  painted: boolean;
}

const JOB_MS = 60_000;

function twoJobs(url: string): string {
  return `name: two-jobs
on: workflow_dispatch
jobs:
  build:
    runs-on: [self-hosted]
    steps:
      - name: Wait for the gate
        run: until curl -fsS --noproxy '*' -o /dev/null "${url}"; do sleep 0.2; done
  test:
    runs-on: [self-hosted]
    needs: build
    steps:
      - run: echo "test runs after build"
`;
}

const SLEEP = `name: sleep
on: workflow_dispatch
jobs:
  sleep:
    runs-on: [self-hosted]
    steps:
      - run: exec sleep 600
`;

function diamond(url: string): string {
  return `name: diamond
on: workflow_dispatch
jobs:
  prepare:
    runs-on: [self-hosted]
    steps:
      - name: Wait for the gate
        run: until curl -fsS --noproxy '*' -o /dev/null "${url}"; do sleep 0.2; done
  build:
    runs-on: [self-hosted]
    needs: prepare
    steps:
      - run: echo "build runs after prepare"
  lint:
    runs-on: [self-hosted]
    needs: prepare
    steps:
      - run: echo "lint runs after prepare"
  ship:
    runs-on: [self-hosted]
    needs: [build, lint]
    steps:
      - run: echo "ship runs after build and lint"
`;
}

const DEPENDENCIES = [
  ['build', 'prepare'],
  ['lint', 'prepare'],
  ['ship', 'build'],
  ['ship', 'lint'],
] as const;

const STUCK = `name: stuck
on: workflow_dispatch
jobs:
  wait:
    runs-on: [self-hosted, e2e-a-label-no-agent-offers]
    steps:
      - run: echo "no agent runs this"
`;

function header(page: Page, title: string) {
  return page.getByRole('region', { name: title });
}

function graph(page: Page) {
  return page.getByRole('application', { name: english('runPage.jobs.title') });
}

function job(page: Page, name: string) {
  return graph(page).getByRole('group', { name, exact: true });
}

async function paint(node: Locator): Promise<Painted> {
  return node.locator('.job-node').evaluate((element: HTMLElement) => {
    const probe = document.createElement('span');
    probe.style.color = `var(--${element.dataset.tone}-9)`;
    element.append(probe);
    const expected = getComputedStyle(probe).color;
    probe.remove();
    return { tone: element.dataset.tone, painted: getComputedStyle(element).borderLeftColor === expected };
  });
}

test('the jobs of a dispatched workflow turn green without a reload, and cancel stops a running run', async ({
  page,
  playwright,
}, info) => {
  const opening = await gate();
  try {
    const owner = await asOwner(playwright);
    const path = await project(owner, unique(info.project.name));
    const gated = await workflow(owner, path, twoJobs(opening.url));
    const sleeping = await workflow(owner, path, SLEEP);
    const developer = await person(owner, unique(info.project.name), 'developer', path);
    await signIn(page, developer);

    const run = await dispatch(owner, gated);
    await page.goto(`/runs/${run}`);
    await page.evaluate(() => {
      document.documentElement.dataset.e2eLoaded = 'once';
    });
    await expect(job(page, 'build')).toContainText(english('state.status.in_progress'), { timeout: JOB_MS });
    await expect(job(page, 'test')).toContainText(english('state.status.pending'));
    await expect(header(page, 'two-jobs #1')).toContainText(english('state.status.in_progress'));

    opening.open();
    await expect(job(page, 'build')).toContainText(english('state.conclusion.success'), { timeout: JOB_MS });
    await expect(job(page, 'test')).toContainText(english('state.conclusion.success'), { timeout: JOB_MS });
    await expect(header(page, 'two-jobs #1')).toContainText(english('state.conclusion.success'));
    expect(await page.evaluate(() => document.documentElement.dataset.e2eLoaded)).toBe('once');

    const slept = await dispatch(owner, sleeping);
    await page.goto(`/runs/${slept}`);
    await expect(job(page, 'sleep')).toContainText(english('state.status.in_progress'), { timeout: JOB_MS });
    await page.getByRole('button', { name: english('runPage.cancel.run'), exact: true }).click();
    await page
      .getByRole('alertdialog')
      .getByRole('button', { name: english('runPage.cancel.run'), exact: true })
      .click();
    await expect(header(page, 'sleep #1')).toContainText(english('state.conclusion.cancelled'), {
      timeout: JOB_MS,
    });
    await expect(job(page, 'sleep')).toContainText(english('state.conclusion.cancelled'));
    await expect(page.getByRole('button', { name: english('runPage.cancel.run'), exact: true })).toHaveCount(
      0,
    );
    const ended = await created<Run>(owner.get(`/api/v1/runs/${slept}`));
    expect([ended.status, ended.conclusion]).toEqual(['completed', 'cancelled']);
  } finally {
    await opening.close();
  }
});

test('the graph draws an edge per dependency and colours each node as its job completes', async ({
  page,
  playwright,
}, info) => {
  const opening = await gate();
  try {
    const owner = await asOwner(playwright);
    const path = await project(owner, unique(info.project.name));
    const drawn = await workflow(owner, path, diamond(opening.url));
    const viewer = await person(owner, unique(info.project.name), 'viewer', path);
    await signIn(page, viewer);

    const run = await dispatch(owner, drawn);
    await page.goto(`/runs/${run}`);
    await expect(job(page, 'prepare')).toContainText(english('state.status.in_progress'), {
      timeout: JOB_MS,
    });
    await expect.poll(() => paint(job(page, 'prepare'))).toEqual({ tone: 'blue', painted: true });
    for (const name of ['build', 'lint', 'ship']) {
      await expect(job(page, name)).toContainText(english('state.status.pending'));
      await expect.poll(() => paint(job(page, name))).toEqual({ tone: 'gray', painted: true });
    }
    await expect(graph(page).locator('.react-flow__edge')).toHaveCount(DEPENDENCIES.length);
    for (const [dependent, need] of DEPENDENCIES) {
      await expect(
        graph(page).getByRole('img', {
          name: english('runPage.graph.needs', { job: dependent, need }),
          exact: true,
        }),
      ).toHaveCount(1);
    }

    opening.open();
    for (const name of ['prepare', 'build', 'lint', 'ship']) {
      await expect(job(page, name)).toContainText(english('state.conclusion.success'), { timeout: JOB_MS });
      await expect.poll(() => paint(job(page, name))).toEqual({ tone: 'green', painted: true });
    }
    await expect(header(page, 'diamond #1')).toContainText(english('state.conclusion.success'));
    await expect(graph(page).locator('.react-flow__edge')).toHaveCount(DEPENDENCIES.length);
  } finally {
    await opening.close();
  }
});

test('a viewer sees no cancel button, and the API refuses the cancel anyway', async ({
  page,
  playwright,
}, info) => {
  const owner = await asOwner(playwright);
  const path = await project(owner, unique(info.project.name));
  const stuck = await workflow(owner, path, STUCK);
  const viewer = await person(owner, unique(info.project.name), 'viewer', path);
  await signIn(page, viewer);

  const run = await dispatch(owner, stuck);
  const jobs = await created<Jobs>(owner.get(`/api/v1/runs/${run}/attempts/1/jobs`));
  const waiting = jobs.items.find((item) => item.key === 'wait');
  expect(waiting).toBeDefined();
  try {
    await page.goto(`/runs/${run}`);
    await expect(job(page, 'wait')).toContainText(english('state.status.queued'));
    await job(page, 'wait').getByRole('link', { name: 'wait', exact: true }).click();
    await expect(page.getByRole('heading', { name: 'wait', level: 3 })).toBeVisible();
    await expect(header(page, 'stuck #1')).toContainText(english('state.status.queued'));
    await expect(page.getByRole('button', { name: english('runPage.cancel.run') })).toHaveCount(0);
    await expect(page.getByRole('button', { name: english('runPage.cancel.job') })).toHaveCount(0);

    const commands = [
      { type: 'run.cancel', target: NO_TARGET, reason: null },
      { type: 'job.cancel', target: { ...NO_TARGET, job: waiting?.id ?? null }, reason: null },
    ];
    const answers = await page.evaluate(
      async ({ id, sent }) => {
        const me: Me = await (await fetch('/api/v1/me')).json();
        const answered: Answer[] = [];
        for (const command of sent) {
          const response = await fetch(`/api/v1/runs/${id}/commands`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'X-Bosun-CSRF': me.csrf_token },
            body: JSON.stringify(command),
          });
          const refusal: Refusal = await response.json();
          answered.push({ status: response.status, code: refusal.error.code });
        }
        return answered;
      },
      { id: run, sent: commands },
    );
    expect(answers).toEqual([
      { status: 403, code: 'forbidden' },
      { status: 403, code: 'forbidden' },
    ]);
    const kept = await created<Run>(owner.get(`/api/v1/runs/${run}`));
    expect(kept.status).toBe('queued');
  } finally {
    await created(
      owner.post(`/api/v1/runs/${run}/commands`, {
        data: { type: 'run.cancel', target: NO_TARGET, reason: null },
      }),
    );
  }
});
