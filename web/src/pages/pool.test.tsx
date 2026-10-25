import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import me from '../../../contracts/fixtures/api/me.json';
import pool from '../../../contracts/fixtures/api/pool.json';
import created from '../../../contracts/fixtures/api/registration-token-created.json';
import { installCommand } from '../agents/registration';
import type { Me, Pool, RegistrationTokenRequest } from '../api/client';
import { translate } from '../i18n/i18n';
import { json, ORIGIN, renderAt } from '../test/render';

const owner: Me = {
  ...(me as Me),
  user: { ...me.user, login: 'owner', owner: true },
  permissions: { installation: ['project.view', 'agent.manage'], organisations: {}, projects: {} },
};

const forks: Pool = {
  ...(pool as Pool),
  id: '0192f3a4-5b6c-7d8e-9f01-000000000024',
  name: 'forks',
  trust: 'untrusted',
};

const HOUR_MS = 3_600_000;

function path(request: Request): string {
  return `${request.method} ${request.url.slice(ORIGIN.length).split('?')[0]}`;
}

function serve(who: Me, requests: Request[] = []) {
  return (request: Request) => {
    requests.push(request.clone());
    switch (path(request)) {
      case 'GET /api/v1/me':
        return json(200, who);
      case 'GET /api/v1/events':
        return new Response('id: 1.1.1.1\nretry: 1000\n\n', {
          headers: { 'Content-Type': 'text/event-stream' },
        });
      case 'GET /api/v1/pools':
        return json(200, { items: [pool, forks], next: null });
      case 'GET /api/v1/agents':
        return json(200, { items: [], next: null });
      case 'POST /api/v1/registration-tokens':
        return json(201, { ...created, pool_id: forks.id });
      default:
        return json(404, {
          error: { code: 'not_found', message: path(request), fields: [], diagnostics: [] },
        });
    }
  };
}

describe('the pool page', () => {
  it('says what an untrusted pool cannot promise', async () => {
    renderAt('/pools/forks', serve(owner));

    expect(await screen.findByRole('heading', { name: 'forks', level: 1 })).toBeVisible();
    expect(screen.getByRole('note')).toHaveTextContent(translate('en', 'poolPage.untrusted'));
    expect(screen.getByText(translate('en', 'agentsPage.trust.untrusted'))).toBeVisible();
    expect(await screen.findByText(translate('en', 'agentsPage.empty'))).toBeVisible();
  });

  it('gives a trusted pool no such warning', async () => {
    renderAt(`/pools/${pool.name}`, serve(owner));
    expect(await screen.findByRole('heading', { name: pool.name, level: 1 })).toBeVisible();
    expect(screen.queryByRole('note')).toBeNull();
    expect(screen.getByText('globalart, ownlate/platform')).toBeVisible();
  });

  it('refuses to show pools to those who do not manage agents', async () => {
    const requests: Request[] = [];
    renderAt('/pools/forks', serve(me as Me, requests));
    expect(await screen.findByText(translate('en', 'poolPage.notAllowed'))).toBeVisible();
    expect(requests.some((request) => path(request) === 'GET /api/v1/pools')).toBe(false);
  });

  it('shows the install command of a new registration token once', async () => {
    const requests: Request[] = [];
    const { router } = renderAt('/pools/forks', serve(owner, requests));
    const user = userEvent.setup();
    const command = installCommand(window.location.origin, created.token);

    await user.click(await screen.findByRole('button', { name: translate('en', 'poolPage.token.create') }));

    expect(await screen.findByLabelText(translate('en', 'poolPage.token.label'))).toHaveTextContent(command);
    expect(command).toBe(
      `curl -fsSL ${window.location.origin}/install-agent.sh | sudo sh -s -- --token ${created.token}`,
    );
    const sent = requests.find((request) => path(request) === 'POST /api/v1/registration-tokens');
    const body = (await sent?.json()) as RegistrationTokenRequest;
    expect(body.pool).toBe('forks');
    expect(body.reusable).toBe(false);
    expect(Date.parse(body.expires_at ?? '') - Date.now()).toBeGreaterThan(23 * HOUR_MS);
    expect(Date.parse(body.expires_at ?? '') - Date.now()).toBeLessThanOrEqual(24 * HOUR_MS);

    await user.click(screen.getByRole('button', { name: translate('en', 'poolPage.token.done') }));
    expect(screen.queryByText(command)).toBeNull();

    await user.click(screen.getByRole('button', { name: translate('en', 'poolPage.token.create') }));
    expect(await screen.findByText(command)).toBeVisible();
    await router.navigate('/queue');
    expect(
      await screen.findByRole('heading', { name: translate('en', 'shell.nav.agents'), level: 1 }),
    ).toBeVisible();
    await router.navigate('/pools/forks');
    expect(await screen.findByRole('heading', { name: 'forks', level: 1 })).toBeVisible();
    await waitFor(() => expect(screen.queryByText(command)).toBeNull());
  });
});
