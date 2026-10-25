import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import me from '../../../contracts/fixtures/api/me.json';
import organisation from '../../../contracts/fixtures/api/organisation.json';
import project from '../../../contracts/fixtures/api/project.json';
import secret from '../../../contracts/fixtures/api/secret.json';
import variable from '../../../contracts/fixtures/api/variable.json';
import type { NamedValue, Secret } from '../api/client';
import { translate } from '../i18n/i18n';
import { json, ORIGIN, renderAt } from '../test/render';

const VALUE = 'a value nobody reads back';

function path(request: Request): string {
  return `${request.method} ${request.url.slice(ORIGIN.length).split('?')[0]}`;
}

function scope(request: Request): string {
  const query = new URL(request.url).searchParams;
  return query.get('project') ?? `organisation ${query.get('organisation')}`;
}

function serve(requests: Request[] = []) {
  return async (request: Request) => {
    requests.push(request.clone());
    switch (path(request)) {
      case 'GET /api/v1/me':
        return json(200, me);
      case 'GET /api/v1/projects':
        return json(200, { items: [project], next: null });
      case `GET /api/v1/projects/${project.path}`:
        return json(200, project);
      case `GET /api/v1/organisations/${organisation.id}`:
      case `GET /api/v1/organisations/${organisation.slug}`:
        return json(200, organisation);
      case 'GET /api/v1/secrets':
        return json(200, { items: scope(request) === project.id ? [secret] : [], next: null });
      case 'GET /api/v1/variables':
        return json(200, {
          items: [{ ...variable, project_id: scope(request) === project.id ? project.id : null }],
          next: null,
        });
      case 'POST /api/v1/secrets': {
        const sent = (await request.json()) as NamedValue;
        const stored: Secret = { ...secret, id: '0192f3a4-5b6c-7d8e-9f01-000000000033', name: sent.name };
        return json(201, stored);
      }
      default:
        return json(404, {
          error: { code: 'not_found', message: path(request), fields: [], diagnostics: [] },
        });
    }
  };
}

describe('the settings', () => {
  it('list the organisations and the projects whose settings the user can see', async () => {
    renderAt('/settings', serve());

    const organisations = await screen.findByRole('region', {
      name: translate('en', 'settingsPage.organisations.title'),
    });
    expect(await within(organisations).findByRole('link', { name: organisation.name })).toHaveAttribute(
      'href',
      `/settings/${organisation.slug}`,
    );
    const projects = screen.getByRole('region', { name: translate('en', 'home.projects.title') });
    expect(within(projects).getByRole('link', { name: project.name })).toHaveAttribute(
      'href',
      `/projects/${project.path}/settings`,
    );
  });

  it('show only the names and dates of secrets, and set a secret for a maintainer', async () => {
    const requests: Request[] = [];
    renderAt(`/projects/${project.path}/settings`, serve(requests));
    const user = userEvent.setup();

    const secrets = await screen.findByRole('region', {
      name: translate('en', 'settingsPage.secrets.title'),
    });
    expect(await within(secrets).findByText(secret.name)).toBeVisible();
    expect(
      within(secrets)
        .getAllByRole('columnheader')
        .map((header) => header.textContent),
    ).toEqual([
      translate('en', 'settingsPage.column.name'),
      translate('en', 'runs.column.created'),
      translate('en', 'settingsPage.column.updated'),
      translate('en', 'form.actions'),
    ]);
    expect(
      within(secrets).getByRole('button', { name: translate('en', 'settingsPage.secrets.replace') }),
    ).toBeVisible();

    const form = within(secrets).getByRole('form', {
      name: translate('en', 'settingsPage.secrets.setTitle'),
    });
    await user.type(within(form).getByLabelText(translate('en', 'settingsPage.column.name')), 'npm_token');
    await user.type(within(form).getByLabelText(translate('en', 'settingsPage.column.value')), VALUE);
    await user.click(within(form).getByRole('button', { name: translate('en', 'settingsPage.secrets.set') }));

    expect(
      await within(secrets).findByText(translate('en', 'settingsPage.secrets.saved', { name: 'npm_token' })),
    ).toBeVisible();
    await waitFor(() => expect(within(secrets).getAllByRole('row')).toHaveLength(3));
    expect(within(secrets).getAllByRole('row')[2]).toHaveTextContent('npm_token');
    expect(document.body).not.toHaveTextContent(VALUE);
    const posted = requests.find((request) => path(request) === 'POST /api/v1/secrets');
    expect(new URL(posted?.url ?? '').searchParams.get('project')).toBe(project.id);
    expect(await posted?.json()).toEqual({ name: 'npm_token', value: VALUE });
    expect(
      within(
        screen.getByRole('form', { name: translate('en', 'settingsPage.secrets.setTitle') }),
      ).getByLabelText(translate('en', 'settingsPage.column.value')),
    ).toHaveValue('');
  });

  it('offer no control to write a secret or a variable to those who may not', async () => {
    renderAt(`/settings/${organisation.slug}`, serve());

    expect(
      await screen.findByRole('heading', {
        name: translate('en', 'settingsPage.of', { name: organisation.name }),
      }),
    ).toBeVisible();
    const secrets = screen.getByRole('region', { name: translate('en', 'settingsPage.secrets.title') });
    expect(await within(secrets).findByText(translate('en', 'settingsPage.secrets.empty'))).toBeVisible();
    expect(within(secrets).queryByRole('form')).toBeNull();
    const variables = screen.getByRole('region', { name: translate('en', 'settingsPage.variables.title') });
    expect(await within(variables).findByText(variable.value)).toBeVisible();
    expect(within(variables).queryByRole('form')).toBeNull();
    expect(within(variables).queryByRole('button')).toBeNull();
    expect(
      within(variables).queryByRole('columnheader', { name: translate('en', 'form.actions') }),
    ).toBeNull();
  });
});
