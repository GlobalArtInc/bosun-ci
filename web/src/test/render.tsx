import { render } from '@testing-library/react';
import { createMemoryRouter } from 'react-router';
import { Api } from '../api/client';
import { App, routes } from '../app';

export const ORIGIN = 'http://bosun.test';

type Server = (request: Request) => Response | Promise<Response>;

interface Rendered {
  router: ReturnType<typeof createMemoryRouter>;
  requests: Request[];
}

export function json(status: number, body: unknown, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json', ...headers },
  });
}

export function envelope(status: number, code: string, message: string): Response {
  return json(status, { error: { code, message, fields: [], diagnostics: [] } });
}

export function renderAt(path: string, server: Server): Rendered {
  const requests: Request[] = [];
  const api = new Api({
    baseUrl: ORIGIN,
    fetch: async (request) => {
      requests.push(request.clone());
      return server(request);
    },
  });
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  render(<App api={api} router={router} />);
  return { router, requests };
}
