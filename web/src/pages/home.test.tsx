import { screen, waitFor, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import me from '../../../contracts/fixtures/api/me.json';
import project from '../../../contracts/fixtures/api/project.json';
import queued from '../../../contracts/fixtures/api/queued-job.json';
import run from '../../../contracts/fixtures/api/run.json';
import workflow from '../../../contracts/fixtures/api/workflow.json';
import entry from '../../../contracts/fixtures/events/queue-job.json';
import { translate } from '../i18n/i18n';
import { json, ORIGIN, renderAt } from '../test/render';

const passing = { ...workflow, id: '0192f3a4-5b6c-7d8e-9f01-000000000015', name: 'deploy' };

const failing = { ...workflow, latest_run: run };

function serve(requests: Request[]) {
  return (request: Request) => {
    requests.push(request);
    const path = new URL(request.url).pathname;
    switch (path) {
      case '/api/v1/me':
        return json(200, me);
      case '/api/v1/events':
        return new Response('id: 1.1.1.1\nretry: 1000\n\n', {
          headers: { 'Content-Type': 'text/event-stream' },
        });
      case '/api/v1/projects':
        return json(200, { items: [project], next: null });
      case `/api/v1/projects/${project.id}/workflows`:
        return json(200, { items: [failing, passing], next: null });
      case '/api/v1/runs':
        return json(200, { items: [run], next: null });
      case '/api/v1/queue':
        return json(200, { items: [queued, entry.data], next: null });
      default:
        return json(404, { error: { code: 'not_found', message: path, fields: [], diagnostics: [] } });
    }
  };
}

describe('the home page', () => {
  it('shows the recent runs, the queue in short, the failing workflows and the projects', async () => {
    const requests: Request[] = [];
    renderAt('/', serve(requests));

    const recent = await screen.findByRole('region', { name: translate('en', 'home.recent.title') });
    const link = await within(recent).findByRole('link', { name: `${run.display_name} #${run.number}` });
    expect(link).toHaveAttribute('href', `/runs/${run.id}`);
    expect(within(recent).getByRole('link', { name: project.path })).toHaveAttribute(
      'href',
      `/projects/${project.path}`,
    );
    expect(within(recent).getByRole('row', { name: new RegExp(`${run.display_name} #`) })).toHaveTextContent(
      translate('en', 'state.conclusion.failure'),
    );

    const queue = screen.getByRole('region', { name: translate('en', 'home.queue.title') });
    expect(await within(queue).findByText(translate('en', 'home.queue.queued', { count: 1 }))).toBeVisible();
    expect(within(queue).getByText(translate('en', 'home.queue.waiting', { count: 1 }))).toBeVisible();
    expect(within(queue).getByText(translate('en', 'home.queue.missing', { labels: 'gpu' }))).toBeVisible();

    const failingList = screen.getByRole('region', { name: translate('en', 'home.failing.title') });
    expect(
      await within(failingList).findByRole('link', { name: `${failing.name} #${run.number}` }),
    ).toHaveAttribute('href', `/runs/${run.id}`);
    expect(within(failingList).queryByText(new RegExp(passing.name))).toBeNull();

    const projects = screen.getByRole('region', { name: translate('en', 'home.projects.title') });
    expect(within(projects).getByRole('link', { name: project.name })).toHaveAttribute(
      'href',
      `/projects/${project.path}`,
    );

    await waitFor(() =>
      expect(requests.some((request) => request.url.includes('/api/v1/events'))).toBe(true),
    );
    const stream = requests.find((request) => request.url.startsWith(`${ORIGIN}/api/v1/events`));
    expect(new URL(stream?.url ?? '').searchParams.getAll('kind')).toEqual([
      'run.created',
      'run.status',
      'queue.job',
    ]);
    const runs = requests.find((request) => new URL(request.url).pathname === '/api/v1/runs');
    expect(new URL(runs?.url ?? '').searchParams.get('limit')).toBe('15');
  });

  it('says so when nothing fails and no job waits', async () => {
    renderAt('/', (request) => {
      const path = new URL(request.url).pathname;
      if (path === '/api/v1/queue' || path === '/api/v1/runs') {
        return json(200, { items: [], next: null });
      }
      if (path === `/api/v1/projects/${project.id}/workflows`) {
        return json(200, { items: [passing], next: null });
      }
      return serve([])(request);
    });

    expect(await screen.findByText(translate('en', 'home.queue.empty'))).toBeVisible();
    expect(await screen.findByText(translate('en', 'home.failing.empty'))).toBeVisible();
    expect(await screen.findByText(translate('en', 'runs.empty'))).toBeVisible();
  });
});
