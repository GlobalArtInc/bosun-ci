import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import me from '../../../contracts/fixtures/api/me.json';
import { translate } from '../i18n/i18n';
import { envelope, json, ORIGIN, renderAt } from '../test/render';

function path(request: Request): string {
  return `${request.method} ${request.url.slice(ORIGIN.length)}`;
}

describe('the shell', () => {
  it('sends a visitor without a session to sign in and back', async () => {
    const { router } = renderAt('/runs/42', (request) =>
      path(request) === 'GET /api/v1/me'
        ? envelope(401, 'unauthenticated', 'the request carries neither a bearer token nor a session cookie')
        : json(200, { password: true, providers: [] }),
    );

    await waitFor(() => expect(router.state.location.pathname).toBe('/sign-in'));
    expect(router.state.location.search).toBe('?next=%2Fruns%2F42');
  });

  it('switches the language and the theme', async () => {
    renderAt('/', () => json(200, me));
    const user = userEvent.setup();

    expect(await screen.findByRole('heading', { name: translate('en', 'home.title') })).toBeInTheDocument();
    expect(screen.getByRole('navigation', { name: translate('en', 'shell.nav.label') })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: translate('en', 'shell.theme.toDark') }));
    expect(screen.getByRole('button', { name: translate('en', 'shell.theme.toLight') })).toBeInTheDocument();
    expect(document.querySelector('.radix-themes')).toHaveClass('dark');

    await user.click(screen.getByRole('combobox', { name: translate('en', 'shell.language.label') }));
    await user.click(await screen.findByRole('option', { name: 'Русский' }));

    expect(await screen.findByRole('heading', { name: translate('ru', 'home.title') })).toBeInTheDocument();
    expect(document.documentElement.lang).toBe('ru');
  });

  it('signs out from the user menu', async () => {
    let signedIn = true;
    const { router, requests } = renderAt('/', (request) => {
      if (path(request) === 'POST /api/v1/auth/logout') {
        signedIn = false;
        return new Response(null, { status: 204 });
      }
      if (path(request) === 'GET /api/v1/auth/methods') {
        return json(200, { password: true, providers: [] });
      }
      return signedIn ? json(200, me) : envelope(401, 'unauthenticated', 'the session is not known');
    });
    const user = userEvent.setup();

    await user.click(await screen.findByRole('button', { name: me.user.login }));
    expect(
      await screen.findByText(translate('en', 'shell.user.signedInAs', { login: me.user.login })),
    ).toBeVisible();
    await user.click(screen.getByRole('menuitem', { name: translate('en', 'shell.user.signOut') }));

    await waitFor(() => expect(router.state.location.pathname).toBe('/sign-in'));
    const out = requests.find((request) => path(request) === 'POST /api/v1/auth/logout');
    expect(out?.headers.get('X-Bosun-CSRF')).toBe(me.csrf_token);
  });

  it('shows a page for a route the interface does not have', async () => {
    renderAt('/nowhere', () => json(200, me));

    expect(
      await screen.findByRole('heading', { name: translate('en', 'notFound.title') }),
    ).toBeInTheDocument();
    expect(screen.getByText(translate('en', 'notFound.message', { path: '/nowhere' }))).toBeInTheDocument();
  });
});
