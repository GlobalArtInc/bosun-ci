import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import me from '../../../contracts/fixtures/api/me.json';
import methods from '../../../contracts/fixtures/api/sign-in-methods.json';
import { translate } from '../i18n/i18n';
import { envelope, json, ORIGIN, renderAt } from '../test/render';

const UNAUTHENTICATED = 'the request carries neither a bearer token nor a session cookie';

const required = { ...me, password_change_required: true };

function path(request: Request): string {
  return `${request.method} ${request.url.slice(ORIGIN.length)}`;
}

describe('the sign-in page', () => {
  it('offers password sign-in and the providers of /auth/methods', async () => {
    renderAt('/sign-in', (request) =>
      path(request) === 'GET /api/v1/me'
        ? envelope(401, 'unauthenticated', UNAUTHENTICATED)
        : json(200, methods),
    );

    expect(await screen.findByLabelText(translate('en', 'signIn.login'))).toBeInTheDocument();
    expect(screen.getByLabelText(translate('en', 'signIn.password'))).toHaveAttribute('type', 'password');
    const provider = screen.getByRole('link', {
      name: translate('en', 'signIn.provider', { name: 'keycloak' }),
    });
    expect(provider).toHaveAttribute('href', '/api/v1/auth/oidc/keycloak/start');
  });

  it('says so when the server offers no way to sign in', async () => {
    renderAt('/sign-in', (request) =>
      path(request) === 'GET /api/v1/me'
        ? envelope(401, 'unauthenticated', UNAUTHENTICATED)
        : json(200, { password: false, providers: [] }),
    );

    expect(await screen.findByText(translate('en', 'signIn.noMethods'))).toBeInTheDocument();
    expect(screen.queryByLabelText(translate('en', 'signIn.password'))).toBeNull();
  });

  it('reports a wrong password', async () => {
    renderAt('/sign-in', (request) => {
      switch (path(request)) {
        case 'GET /api/v1/auth/methods':
          return json(200, { password: true, providers: [] });
        case 'POST /api/v1/auth/login':
          return envelope(401, 'unauthenticated', 'the login or the password is not right');
        default:
          return envelope(401, 'unauthenticated', UNAUTHENTICATED);
      }
    });
    const user = userEvent.setup();

    await user.type(await screen.findByLabelText(translate('en', 'signIn.login')), 'admin');
    await user.type(screen.getByLabelText(translate('en', 'signIn.password')), 'not the password');
    await user.click(screen.getByRole('button', { name: translate('en', 'signIn.submit') }));

    expect(await screen.findByRole('alert')).toHaveTextContent(translate('en', 'signIn.error.wrong'));
  });

  it('leads a password that must change to the forced change and then home', async () => {
    let signedIn: typeof me | null = null;
    const { router, requests } = renderAt('/sign-in?next=%2Fruns%2F42', (request) => {
      switch (path(request)) {
        case 'GET /api/v1/auth/methods':
          return json(200, { password: true, providers: [] });
        case 'POST /api/v1/auth/login':
          signedIn = required;
          return json(200, required);
        case 'POST /api/v1/auth/password':
          signedIn = me;
          return json(200, me);
        default:
          return signedIn === null ? envelope(401, 'unauthenticated', UNAUTHENTICATED) : json(200, signedIn);
      }
    });
    const user = userEvent.setup();

    await user.type(await screen.findByLabelText(translate('en', 'signIn.login')), 'admin');
    await user.type(screen.getByLabelText(translate('en', 'signIn.password')), 'the first password');
    await user.click(screen.getByRole('button', { name: translate('en', 'signIn.submit') }));
    await waitFor(() => expect(router.state.location.pathname).toBe('/password'));

    await user.type(await screen.findByLabelText(translate('en', 'password.current')), 'the first password');
    await user.type(screen.getByLabelText(translate('en', 'password.new')), 'short');
    await user.type(screen.getByLabelText(translate('en', 'password.repeat')), 'short');
    await user.click(screen.getByRole('button', { name: translate('en', 'password.submit') }));
    expect(await screen.findByRole('alert')).toHaveTextContent(translate('en', 'password.error.short'));
    expect(requests.some((request) => path(request) === 'POST /api/v1/auth/password')).toBe(false);

    await user.clear(screen.getByLabelText(translate('en', 'password.new')));
    await user.type(screen.getByLabelText(translate('en', 'password.new')), 'a longer passphrase of my own');
    await user.clear(screen.getByLabelText(translate('en', 'password.repeat')));
    await user.type(
      screen.getByLabelText(translate('en', 'password.repeat')),
      'a longer passphrase of my own',
    );
    await user.click(screen.getByRole('button', { name: translate('en', 'password.submit') }));

    await waitFor(() => expect(router.state.location.pathname).toBe('/runs/42'));
    const change = requests.find((request) => path(request) === 'POST /api/v1/auth/password');
    expect(change?.headers.get('X-Bosun-CSRF')).toBe(me.csrf_token);
    expect(await change?.json()).toEqual({
      current_password: 'the first password',
      new_password: 'a longer passphrase of my own',
    });
  });
});
