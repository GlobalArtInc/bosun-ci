import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import type { Job, Me, Run, RunEvent } from '../api/client';
import { translate } from '../i18n/i18n';
import {
  developer,
  FORKED,
  forkedJobs,
  forkedRun,
  lintEvents,
  maintainer,
  matrixJobs,
  matrixRun,
  project,
} from '../test/fixtures/graph';
import { json, ORIGIN, renderAt } from '../test/render';

interface Served {
  run: Run;
  jobs: Job[];
  me: Me;
}

interface Stream {
  send: (events: RunEvent[]) => void;
}

function path(request: Request): string {
  return `${request.method} ${request.url.slice(ORIGIN.length).split('?')[0]}`;
}

function serve(served: Served, requests: Request[] = [], stream: Stream = { send: () => undefined }) {
  const encoder = new TextEncoder();
  const open = (request: Request): Response => {
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(encoder.encode('id: 1.1.1.1\nretry: 1000\n\n'));
        stream.send = (events) => {
          for (const event of events) {
            controller.enqueue(
              encoder.encode(`id: ${event.seq}.${event.seq}.1.1\ndata: ${JSON.stringify(event)}\n\n`),
            );
          }
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
        return json(200, served.me);
      case 'GET /api/v1/events':
        return open(request);
      case `GET /api/v1/runs/${served.run.id}`:
        return json(200, served.run);
      case `GET /api/v1/projects/${project.id}`:
        return json(200, project);
      case `GET /api/v1/runs/${served.run.id}/attempts/1/jobs`:
        return json(200, { items: served.jobs, next: null });
      case `POST /api/v1/runs/${served.run.id}/commands`:
        return json(201, { ...lintEvents[0], run_id: served.run.id, kind: 'command.accepted', data: {} });
      default:
        return json(404, {
          error: { code: 'not_found', message: path(request), fields: [], diagnostics: [] },
        });
    }
  };
}

async function graph() {
  return screen.findByRole('application', { name: translate('en', 'runPage.jobs.title') });
}

function nodes(drawn: HTMLElement): HTMLElement[] {
  return [...drawn.querySelectorAll<HTMLElement>('.react-flow__node')];
}

function node(drawn: HTMLElement, name: string): HTMLElement {
  return within(drawn).getByRole('group', { name });
}

function tone(element: HTMLElement): string | undefined {
  return element.querySelector<HTMLElement>('.job-node')?.dataset.tone;
}

describe('the job graph', () => {
  it('collapses a matrix group into one node with its count, and expanding it names each member by its matrix values', async () => {
    renderAt(`/runs/${matrixRun.id}`, serve({ run: matrixRun, jobs: matrixJobs, me: maintainer }));
    const user = userEvent.setup();

    const drawn = await graph();
    await waitFor(() => expect(nodes(drawn)).toHaveLength(4));
    const group = node(drawn, 'test');
    expect(group).toHaveTextContent(translate('en', 'runPage.graph.complete', { done: 1, count: 3 }));
    expect(group).toHaveTextContent(translate('en', 'state.status.in_progress'));
    expect(within(drawn).queryByRole('group', { name: 'test (linux, 20)' })).toBeNull();
    expect(drawn.querySelectorAll('.react-flow__edge')).toHaveLength(4);
    expect(
      within(drawn).getByRole('img', {
        name: translate('en', 'runPage.graph.needs', { job: 'deploy', need: 'test' }),
      }),
    ).toBeInTheDocument();

    await user.click(
      within(group).getByRole('button', { name: translate('en', 'runPage.graph.expand', { name: 'test' }) }),
    );

    await waitFor(() => expect(nodes(drawn)).toHaveLength(7));
    const members = [
      ['test (linux, 20)', 'os: linux, node: 20', 'state.conclusion.success'],
      ['test (linux, 22)', 'os: linux, node: 22', 'state.status.in_progress'],
      ['test (macos, 20)', 'os: macos, node: 20', 'state.waiting.max_parallel'],
    ] as const;
    for (const [name, values, state] of members) {
      const shown = node(drawn, name);
      expect(within(shown).getByRole('link', { name })).toBeInTheDocument();
      expect(shown).toHaveTextContent(values);
      expect(shown).toHaveTextContent(translate('en', state));
    }
    const expanded = node(drawn, 'test');
    const collapse = within(expanded).getByRole('button', {
      name: translate('en', 'runPage.graph.collapse', { name: 'test' }),
    });
    expect(collapse).toHaveAttribute('aria-expanded', 'true');
    expect(collapse).toHaveFocus();
    expect(expanded).toHaveTextContent(translate('en', 'runPage.graph.complete', { done: 1, count: 3 }));
    expect(drawn.querySelectorAll('.react-flow__edge')).toHaveLength(4);

    await user.click(collapse);
    await waitFor(() => expect(nodes(drawn)).toHaveLength(4));
  });

  it('changes the status and the colour of a node when a job event of the stream arrives', async () => {
    const stream: Stream = { send: () => undefined };
    renderAt(
      `/runs/${matrixRun.id}`,
      serve({ run: matrixRun, jobs: matrixJobs, me: maintainer }, [], stream),
    );

    const drawn = await graph();
    await waitFor(() =>
      expect(node(drawn, 'lint')).toHaveTextContent(translate('en', 'state.status.queued')),
    );
    expect(tone(node(drawn, 'lint'))).toBe('gray');

    stream.send(lintEvents.slice(0, 2));
    await waitFor(() =>
      expect(node(drawn, 'lint')).toHaveTextContent(translate('en', 'state.status.in_progress')),
    );
    expect(tone(node(drawn, 'lint'))).toBe('blue');

    stream.send(lintEvents.slice(2));
    await waitFor(() =>
      expect(node(drawn, 'lint')).toHaveTextContent(translate('en', 'state.conclusion.failure')),
    );
    expect(tone(node(drawn, 'lint'))).toBe('red');
    expect(tone(node(drawn, 'build'))).toBe('green');
    expect(tone(node(drawn, 'deploy'))).toBe('gray');
  });

  it('shows the reason a job waits on its node', async () => {
    renderAt(`/runs/${FORKED}`, serve({ run: forkedRun, jobs: forkedJobs, me: developer }));

    const drawn = await graph();
    await waitFor(() =>
      expect(node(drawn, 'build')).toHaveTextContent(translate('en', 'state.waiting.untrusted')),
    );
    expect(tone(node(drawn, 'build'))).toBe('amber');
    expect(node(drawn, 'test')).toHaveTextContent(translate('en', 'state.status.pending'));
  });

  it('offers a maintainer to admit a run that waits for admission, and sends the command after a confirmation', async () => {
    const requests: Request[] = [];
    renderAt(`/runs/${FORKED}`, serve({ run: forkedRun, jobs: forkedJobs, me: maintainer }, requests));
    const user = userEvent.setup();

    await user.click(await screen.findByRole('button', { name: translate('en', 'runPage.admit.run') }));
    const dialog = await screen.findByRole('alertdialog');
    expect(dialog).toHaveTextContent(translate('en', 'runPage.admit.question', { name: 'ci #8' }));
    await user.click(within(dialog).getByRole('button', { name: translate('en', 'runPage.admit.run') }));

    expect(await screen.findByText(translate('en', 'runPage.admit.requested'))).toBeInTheDocument();
    const sent = requests.find((request) => path(request) === `POST /api/v1/runs/${FORKED}/commands`);
    expect(sent?.headers.get('X-Bosun-CSRF')).toBe(maintainer.csrf_token);
    expect(sent?.headers.get('Idempotency-Key')).toMatch(/^[0-9a-f-]{36}$/);
    expect(await sent?.json()).toEqual({
      type: 'run.admit',
      target: { attempt: null, job: null, step: null },
      reason: null,
    });
  });

  it('hides the admit control from a developer', async () => {
    renderAt(`/runs/${FORKED}`, serve({ run: forkedRun, jobs: forkedJobs, me: developer }));

    expect(
      await screen.findByRole('button', { name: translate('en', 'runPage.cancel.run') }),
    ).toBeInTheDocument();
    await graph();
    expect(screen.queryByRole('button', { name: translate('en', 'runPage.admit.run') })).toBeNull();
  });

  it('hides the admit control from a maintainer when no job waits for admission', async () => {
    renderAt(`/runs/${matrixRun.id}`, serve({ run: matrixRun, jobs: matrixJobs, me: maintainer }));

    expect(
      await screen.findByRole('button', { name: translate('en', 'runPage.cancel.run') }),
    ).toBeInTheDocument();
    await graph();
    expect(screen.queryByRole('button', { name: translate('en', 'runPage.admit.run') })).toBeNull();
  });
});
