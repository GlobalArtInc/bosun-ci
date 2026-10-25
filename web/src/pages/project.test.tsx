import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import me from '../../../contracts/fixtures/api/me.json';
import project from '../../../contracts/fixtures/api/project.json';
import run from '../../../contracts/fixtures/api/run.json';
import workflow from '../../../contracts/fixtures/api/workflow.json';
import { translate } from '../i18n/i18n';
import { json, renderAt } from '../test/render';

const unused = {
  ...workflow,
  id: '0192f3a4-5b6c-7d8e-9f01-000000000015',
  name: 'nightly',
  enabled: false,
  latest_run: null,
};

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
      case `/api/v1/projects/${project.path}`:
        return json(200, project);
      case `/api/v1/projects/${project.id}/workflows`:
        return json(200, { items: [workflow, unused], next: null });
      case '/api/v1/runs':
        return json(200, { items: [run], next: null });
      default:
        return json(404, {
          error: { code: 'not_found', message: `there is no ${path}`, fields: [], diagnostics: [] },
        });
    }
  };
}

function queries(requests: Request[]): URLSearchParams[] {
  return requests
    .filter((request) => new URL(request.url).pathname === '/api/v1/runs')
    .map((request) => new URL(request.url).searchParams);
}

describe('the project page', () => {
  it('lists the workflows with the status of their latest run', async () => {
    const requests: Request[] = [];
    renderAt(`/projects/${project.path}`, serve(requests));

    expect(await screen.findByRole('heading', { name: project.name })).toBeInTheDocument();
    const workflows = screen.getByRole('region', { name: translate('en', 'projectPage.workflows.title') });
    const ci = await within(workflows).findByRole('row', { name: new RegExp(workflow.name) });
    expect(ci).toHaveTextContent(translate('en', 'state.conclusion.success'));
    expect(within(ci).getByRole('link', { name: `#${workflow.latest_run.number}` })).toHaveAttribute(
      'href',
      `/runs/${workflow.latest_run.id}`,
    );
    const nightly = within(workflows).getByRole('row', { name: /nightly/ });
    expect(nightly).toHaveTextContent(translate('en', 'projectPage.workflows.never'));
    expect(nightly).toHaveTextContent(translate('en', 'projectPage.workflows.disabled'));

    const stream = requests.find((request) => new URL(request.url).pathname === '/api/v1/events');
    expect(new URL(stream?.url ?? '').searchParams.getAll('project')).toEqual([project.id]);
    await waitFor(() => expect(queries(requests)[0]?.get('project')).toBe(project.id));
  });

  it('filters the runs by workflow, branch, event, status and actor', async () => {
    const requests: Request[] = [];
    const { router } = renderAt(
      `/projects/${project.path}?workflow=${workflow.id}&status=failure`,
      serve(requests),
    );
    const user = userEvent.setup();

    const runs = await screen.findByRole('region', { name: translate('en', 'projectPage.runs.title') });
    expect(
      await within(runs).findByRole('link', { name: `${run.display_name} #${run.number}` }),
    ).toBeVisible();
    const first = queries(requests)[0];
    expect(first?.get('workflow')).toBe(workflow.id);
    expect(first?.get('status')).toBe('failure');

    await user.type(within(runs).getByLabelText(translate('en', 'projectPage.filter.branch')), 'main');
    await user.type(within(runs).getByLabelText(translate('en', 'projectPage.filter.event')), 'push');
    await user.type(within(runs).getByLabelText(translate('en', 'projectPage.filter.actor')), 'roman');
    await user.click(within(runs).getByRole('button', { name: translate('en', 'projectPage.filter.apply') }));

    await waitFor(() => expect(router.state.location.search).toContain('branch=main'));
    await waitFor(() => expect(queries(requests).at(-1)?.get('actor')).toBe('roman'));
    const last = queries(requests).at(-1);
    expect(Object.fromEntries(last ?? [])).toEqual({
      workflow: workflow.id,
      status: 'failure',
      branch: 'main',
      event: 'push',
      actor: 'roman',
      project: project.id,
      limit: '25',
    });

    await user.click(within(runs).getByRole('button', { name: translate('en', 'projectPage.filter.clear') }));
    await waitFor(() => expect(router.state.location.search).toBe(''));
  });

  it('says so for a project that does not exist', async () => {
    renderAt('/projects/nobody/nothing', serve([]));

    expect(
      await screen.findByText(translate('en', 'projectPage.notFound', { path: 'nobody/nothing' })),
    ).toBeVisible();
  });
});
