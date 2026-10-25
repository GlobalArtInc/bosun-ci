import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import agent from '../../../contracts/fixtures/api/agent.json';
import execution from '../../../contracts/fixtures/api/execution.json';
import job from '../../../contracts/fixtures/api/job.json';
import alias from '../../../contracts/fixtures/api/label-alias.json';
import me from '../../../contracts/fixtures/api/me.json';
import organisation from '../../../contracts/fixtures/api/organisation.json';
import pool from '../../../contracts/fixtures/api/pool.json';
import project from '../../../contracts/fixtures/api/project.json';
import queued from '../../../contracts/fixtures/api/queued-job.json';
import entry from '../../../contracts/fixtures/events/queue-job.json';
import type { Agent, Me, Permission, QueuedJob, StreamEvent } from '../api/client';
import { translate } from '../i18n/i18n';
import { json, ORIGIN, renderAt } from '../test/render';

interface Served {
  me: Me;
  queue?: QueuedJob[];
  events?: StreamEvent[];
}

interface Stream {
  release: () => void;
}

const EVERY: Permission[] = [
  'project.view',
  'run.dispatch',
  'run.cancel',
  'secret.manage',
  'variable.manage',
  'agent.manage',
  'organisation.manage',
];

const owner: Me = {
  ...(me as Me),
  user: { ...me.user, login: 'owner', owner: true },
  permissions: { installation: EVERY, organisations: {}, projects: {} },
};

const busy: Agent = { ...(agent as Agent), drain: false };

const offline: Agent = {
  ...busy,
  id: '0192f3a4-5b6c-7d8e-9f01-000000000020',
  name: 'runner1-2',
  online: false,
  execution_id: null,
  version: null,
};

const waiting = entry.data as QueuedJob;

const stuck = queued as QueuedJob;

const untrusted: QueuedJob = {
  ...waiting,
  job: {
    ...waiting.job,
    id: '0192f3a4-5b6c-7d8e-9f01-000000000022',
    name: 'lint',
    group_id: null,
    matrix: null,
    waiting_reason: 'untrusted',
  },
};

const deploy: QueuedJob = {
  ...stuck,
  job: { ...stuck.job, id: '0192f3a4-5b6c-7d8e-9f01-000000000023', name: 'deploy', labels: ['self-hosted'] },
  missing_labels: [],
};

function event(seq: number, data: QueuedJob): StreamEvent {
  return { ...entry, seq, kind: 'queue.job', data } as StreamEvent;
}

function path(request: Request): string {
  return `${request.method} ${request.url.slice(ORIGIN.length).split('?')[0]}`;
}

function serve(served: Served, requests: Request[] = [], stream: Stream = { release: () => undefined }) {
  const encoder = new TextEncoder();
  const open = (request: Request): Response => {
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(encoder.encode('id: 1.1.1.1\nretry: 1000\n\n'));
        stream.release = () => {
          for (const sent of served.events ?? []) {
            controller.enqueue(
              encoder.encode(`id: 1.1.${sent.seq}.${sent.seq}\ndata: ${JSON.stringify(sent)}\n\n`),
            );
          }
        };
        request.signal.addEventListener('abort', () => controller.error(request.signal.reason));
      },
    });
    return new Response(body, { headers: { 'Content-Type': 'text/event-stream' } });
  };
  return async (request: Request) => {
    requests.push(request.clone());
    switch (path(request)) {
      case 'GET /api/v1/me':
        return json(200, served.me);
      case 'GET /api/v1/events':
        return open(request);
      case 'GET /api/v1/queue':
        return json(200, { items: served.queue ?? [], next: null });
      case 'GET /api/v1/projects':
        return json(200, { items: [project], next: null });
      case 'GET /api/v1/agents':
        return json(200, { items: [busy, offline], next: null });
      case 'GET /api/v1/pools':
        return json(200, { items: [pool], next: null });
      case 'GET /api/v1/label-aliases':
        return json(200, { items: [alias], next: null });
      case `GET /api/v1/organisations/${organisation.id}`:
        return json(200, organisation);
      case `GET /api/v1/executions/${execution.id}`:
        return json(200, execution);
      case `GET /api/v1/jobs/${execution.job_id}`:
        return json(200, { ...job, id: execution.job_id, name: 'build' });
      case `PATCH /api/v1/agents/${busy.id}`: {
        const change = (await request.json()) as Partial<Agent>;
        return json(200, { ...busy, ...change });
      }
      default:
        return json(404, {
          error: { code: 'not_found', message: path(request), fields: [], diagnostics: [] },
        });
    }
  };
}

function row(table: HTMLElement, name: string): HTMLElement {
  const found = within(table)
    .getAllByRole('row')
    .find((candidate) => within(candidate).queryByText(name, { exact: true }) !== null);
  if (found === undefined) {
    throw new Error(`no row holds ${name}`);
  }
  return found;
}

describe('the queue and agents page', () => {
  it('renders the reasons and the missing labels that the queue events of the stream carry', async () => {
    const requests: Request[] = [];
    const stream: Stream = { release: () => undefined };
    renderAt(
      '/queue',
      serve(
        {
          me: me as Me,
          events: [
            event(4, waiting),
            event(5, stuck),
            event(6, untrusted),
            event(7, deploy),
            event(8, { ...deploy, job: { ...deploy.job, status: 'assigned' }, since: null }),
          ],
        },
        requests,
        stream,
      ),
    );

    const table = await screen.findByRole('region', { name: translate('en', 'queuePage.queue.title') });
    expect(await within(table).findByText(translate('en', 'home.queue.empty'))).toBeVisible();
    stream.release();
    await within(table).findByText('lint');
    expect(row(table, 'build')).toHaveTextContent(
      translate('en', 'queuePage.reason.missing', { labels: 'gpu' }),
    );
    expect(within(row(table, 'build')).getByText('gpu')).toBeVisible();
    expect(row(table, 'test (linux)')).toHaveTextContent(translate('en', 'state.waiting.max_parallel'));
    expect(row(table, 'lint')).toHaveTextContent(translate('en', 'state.waiting.untrusted'));
    expect(row(table, 'lint')).toHaveTextContent(translate('en', 'state.status.waiting'));
    expect(row(table, 'build')).toHaveTextContent(translate('en', 'state.status.queued'));
    expect(within(row(table, 'build')).getByRole('link', { name: 'build' })).toHaveAttribute(
      'href',
      `/runs/${stuck.job.run_id}?job=${stuck.job.id}`,
    );
    expect(row(table, 'build')).toHaveTextContent(project.path);
    expect(within(table).queryByText('deploy')).toBeNull();

    expect(screen.getByRole('heading', { name: translate('en', 'shell.nav.queue'), level: 1 })).toBeVisible();
    expect(screen.queryByRole('region', { name: translate('en', 'queuePage.agents.title') })).toBeNull();
    const followed = requests.find((request) => request.url.startsWith(`${ORIGIN}/api/v1/events`));
    expect(new URL(followed?.url ?? '').searchParams.getAll('kind')).toEqual(['queue.job']);
  });

  it('shows the agents by pool to those who manage them, and drains one', async () => {
    const requests: Request[] = [];
    renderAt('/queue', serve({ me: owner, queue: [stuck] }, requests));
    const user = userEvent.setup();

    const agents = await screen.findByRole('region', { name: translate('en', 'queuePage.agents.title') });
    const linux = await within(agents).findByRole('region', { name: pool.name });
    const running = row(linux, busy.name);
    expect(running).toHaveTextContent(translate('en', 'agentsPage.state.busy'));
    expect(running).toHaveTextContent('0.1.0');
    expect(running).toHaveTextContent('self-hosted');
    expect(running).toHaveTextContent('shell');
    expect(await within(running).findByRole('link', { name: 'build' })).toHaveAttribute(
      'href',
      `/runs/${job.run_id}?job=${execution.job_id}`,
    );
    expect(row(linux, offline.name)).toHaveTextContent(translate('en', 'agentsPage.state.offline'));
    expect(running).not.toHaveTextContent(translate('en', 'agentsPage.state.draining'));

    await user.click(within(running).getByRole('button', { name: translate('en', 'agentsPage.drain') }));

    await waitFor(() =>
      expect(row(linux, busy.name)).toHaveTextContent(translate('en', 'agentsPage.state.draining')),
    );
    expect(
      within(row(linux, busy.name)).getByRole('button', { name: translate('en', 'agentsPage.lift') }),
    ).toBeVisible();
    const patch = requests.find((request) => request.method === 'PATCH');
    expect(await patch?.json()).toEqual({ drain: true });

    const pools = screen.getByRole('region', { name: translate('en', 'queuePage.pools.title') });
    expect(within(pools).getByRole('link', { name: pool.name })).toHaveAttribute(
      'href',
      `/pools/${pool.name}`,
    );
    expect(row(pools, pool.name)).toHaveTextContent('globalart, ownlate/platform');
    expect(row(pools, pool.name)).toHaveTextContent(
      translate('en', 'poolPage.agentCount', { online: 1, count: 2 }),
    );
    const aliases = screen.getByRole('region', { name: translate('en', 'queuePage.aliases.title') });
    const aliased = row(aliases, alias.label);
    expect(await within(aliased).findByText(organisation.slug)).toBeVisible();
    expect(aliased).toHaveTextContent(pool.name);
    expect(aliased).toHaveTextContent(alias.image);

    const stream = requests.find((request) => request.url.startsWith(`${ORIGIN}/api/v1/events`));
    expect(new URL(stream?.url ?? '').searchParams.getAll('kind')).toEqual([
      'queue.job',
      'agent.status',
      'execution.created',
      'execution.status',
    ]);
  });
});
