import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import execution from '../../../contracts/fixtures/api/execution.json';
import me from '../../../contracts/fixtures/api/me.json';
import jobs from '../../../contracts/fixtures/api/page-jobs.json';
import project from '../../../contracts/fixtures/api/project.json';
import run from '../../../contracts/fixtures/api/run.json';
import notice from '../../../contracts/fixtures/events/run-notice.json';
import type { Me, Notice, Run, RunEvent, Step } from '../api/client';
import { translate } from '../i18n/i18n';
import { json, ORIGIN, renderAt } from '../test/render';

interface Served {
  run?: Run;
  me?: Me;
  steps?: Step[];
  events?: RunEvent[];
}

const running: Run = {
  ...(run as Run),
  status: 'in_progress',
  conclusion: null,
  completed_at: null,
  notices: [],
};

const viewer: Me = {
  ...(me as Me),
  permissions: { installation: [], organisations: {}, projects: { [project.id]: ['project.view'] } },
};

const BUILD = jobs.items[0]?.id ?? '';

function step(index: number, name: string): Step {
  return {
    ...execution.steps[0],
    id: `0192f3a4-5b6c-7d8e-9f01-0000000002${index.toString().padStart(2, '0')}`,
    index,
    name,
    status: 'completed',
    outcome: 'success',
    conclusion: 'success',
    conclusion_source: 'natural',
  } as Step;
}

function path(request: Request): string {
  return `${request.method} ${request.url.slice(ORIGIN.length).split('?')[0]}`;
}

function serve(served: Served, requests: Request[] = []) {
  const shown = served.run ?? running;
  const encoder = new TextEncoder();
  let release = () => undefined;
  const open = (request: Request): Response => {
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(encoder.encode('id: 1.1.1.1\nretry: 1000\n\n'));
        release = () => {
          for (const event of served.events ?? []) {
            const message = `id: ${event.seq}.${event.seq}.1.1\ndata: ${JSON.stringify(event)}\n\n`;
            controller.enqueue(encoder.encode(message));
          }
          return undefined;
        };
        request.signal.addEventListener('abort', () => controller.error(request.signal.reason));
      },
    });
    return new Response(body, { headers: { 'Content-Type': 'text/event-stream' } });
  };
  return (request: Request) => {
    requests.push(request.clone());
    switch (path(request)) {
      case 'GET /api/v1/me':
        return json(200, served.me ?? me);
      case 'GET /api/v1/events':
        return open(request);
      case `GET /api/v1/runs/${shown.id}`:
        return json(200, shown);
      case `GET /api/v1/projects/${project.id}`:
        return json(200, project);
      case `GET /api/v1/runs/${shown.id}/attempts/1/jobs`:
        release();
        return json(200, { items: jobs.items, next: null });
      case `GET /api/v1/executions/${execution.id}`:
        return json(200, { ...execution, steps: served.steps ?? execution.steps });
      case `POST /api/v1/runs/${shown.id}/commands`:
        return json(201, { ...notice, kind: 'command.accepted', data: { command: {} } });
      default:
        return json(404, {
          error: { code: 'not_found', message: path(request), fields: [], diagnostics: [] },
        });
    }
  };
}

describe('the run page', () => {
  it('lists the notices of the run, the error before the warning, each with its file and line', async () => {
    const warned: Run = {
      ...(run as Run),
      notices: [
        ...(run as Run).notices,
        { job_id: notice.job_id, ...(notice.data as Omit<Notice, 'job_id'>) },
      ],
    };
    renderAt(`/runs/${run.id}`, serve({ run: warned }));

    const list = await screen.findByRole('list', { name: translate('en', 'runPage.notices.title') });
    const [first, second] = within(list).getAllByRole('listitem');
    expect(first).toHaveTextContent(translate('en', 'runPage.notices.error'));
    expect(first).toHaveTextContent('ci:15:26');
    expect(first).toHaveTextContent(notice.data.message);
    await waitFor(() =>
      expect(first).toHaveTextContent(translate('en', 'runPage.notices.job', { name: 'test' })),
    );
    expect(second).toHaveTextContent(translate('en', 'runPage.notices.warning'));
    expect(second).toHaveTextContent('ci:8:13');
    expect(second).toHaveTextContent(run.notices[0]?.message ?? '');
  });

  it('shows the header, and the jobs in the graph with their status and the reason a job waits', async () => {
    const waiting = { ...jobs.items[2], status: 'waiting', waiting_reason: 'concurrency' };
    renderAt(`/runs/${run.id}`, (request) =>
      path(request) === `GET /api/v1/runs/${run.id}/attempts/1/jobs`
        ? json(200, { items: [jobs.items[0], jobs.items[1], waiting], next: null })
        : serve({ run: run as Run })(request),
    );

    const header = await screen.findByRole('region', { name: `${run.display_name} #${run.number}` });
    expect(header).toHaveTextContent(translate('en', 'state.conclusion.failure'));
    expect(header).toHaveTextContent('workflow_dispatch');
    expect(header).toHaveTextContent('roman');
    expect(header).toHaveTextContent(translate('en', 'time.seconds', { seconds: 17 }));
    const graph = await screen.findByRole('application', { name: translate('en', 'runPage.jobs.title') });
    await waitFor(() =>
      expect(within(graph).getByRole('group', { name: 'build' })).toHaveTextContent(
        translate('en', 'state.conclusion.success'),
      ),
    );
    expect(within(graph).getByRole('group', { name: 'test' })).toHaveTextContent(
      translate('en', 'state.waiting.concurrency'),
    );
  });

  it('offers cancel to a developer and sends the command after a confirmation', async () => {
    const requests: Request[] = [];
    renderAt(`/runs/${run.id}`, serve({}, requests));
    const user = userEvent.setup();

    await user.click(await screen.findByRole('button', { name: translate('en', 'runPage.cancel.run') }));
    const dialog = await screen.findByRole('alertdialog');
    await user.click(within(dialog).getByRole('button', { name: translate('en', 'runPage.cancel.run') }));

    expect(await screen.findByText(translate('en', 'runPage.cancel.requested'))).toBeInTheDocument();
    const sent = requests.find((request) => path(request) === `POST /api/v1/runs/${run.id}/commands`);
    expect(sent?.headers.get('X-Bosun-CSRF')).toBe(me.csrf_token);
    expect(sent?.headers.get('Idempotency-Key')).toMatch(/^[0-9a-f-]{36}$/);
    expect(await sent?.json()).toEqual({
      type: 'run.cancel',
      target: { attempt: null, job: null, step: null },
      reason: null,
    });
  });

  it('shows no cancel to a viewer', async () => {
    renderAt(`/runs/${run.id}?job=${jobs.items[2]?.id}`, serve({ me: viewer }));

    expect(await screen.findByRole('heading', { name: 'test (linux, 20)' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: translate('en', 'runPage.cancel.run') })).toBeNull();
    expect(screen.queryByRole('button', { name: translate('en', 'runPage.cancel.job') })).toBeNull();
  });

  it('nests the pre and post steps of an action under the step that uses it', async () => {
    renderAt(
      `/runs/${run.id}?job=${BUILD}`,
      serve({
        steps: [
          step(0, 'Pre Run actions/cache@v4'),
          step(1, 'Run actions/checkout@v4'),
          step(2, 'Run actions/cache@v4'),
          step(3, 'Build'),
          step(4, 'Post Run actions/cache@v4'),
        ],
      }),
    );

    const steps = await screen.findByRole('list', { name: translate('en', 'runPage.panel.steps') });
    const tops = [...steps.children].map((item) => item.firstElementChild?.textContent ?? '');
    expect(tops).toEqual([
      expect.stringMatching(/^Run actions\/checkout@v4\d/),
      expect.stringMatching(/^Run actions\/cache@v4\d/),
      expect.stringMatching(/^Build\d/),
    ]);
    const cache = steps.children[1];
    expect(cache === undefined ? [] : within(cache as HTMLElement).getAllByRole('listitem')).toHaveLength(2);
    expect(cache).toHaveTextContent('Pre Run actions/cache@v4');
    expect(cache).toHaveTextContent('Post Run actions/cache@v4');
  });

  it('follows the run live: a status event of the stream changes the header without a reload', async () => {
    const completed: RunEvent = {
      seq: 30,
      run_id: run.id,
      attempt_id: jobs.items[0]?.attempt_id ?? null,
      job_id: null,
      execution_id: null,
      step_id: null,
      actor: { kind: 'system' },
      at: '2026-10-24T18:35:07.512Z',
      kind: 'run.status',
      data: { from: 'in_progress', to: 'completed', conclusion: 'success' },
    };
    const requests: Request[] = [];
    renderAt(`/runs/${run.id}`, serve({ events: [completed] }, requests));

    const header = await screen.findByRole('region', { name: `${run.display_name} #${run.number}` });
    await waitFor(() => expect(header).toHaveTextContent(translate('en', 'state.conclusion.success')));
    expect(screen.queryByRole('button', { name: translate('en', 'runPage.cancel.run') })).toBeNull();
    const stream = requests.find((request) => path(request) === 'GET /api/v1/events');
    expect(new URL(stream?.url ?? '').searchParams.getAll('run')).toEqual([run.id]);
  });
});
