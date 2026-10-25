import { randomBytes } from 'node:crypto';
import { createServer } from 'node:http';
import type { AddressInfo } from 'node:net';
import {
  type APIRequestContext,
  type APIResponse,
  expect,
  type Page,
  type PlaywrightWorkerArgs,
} from '@playwright/test';
import { english } from './environment';

interface Created {
  id: string;
}

interface Workflow extends Created {
  name: string;
}

interface Person {
  login: string;
  password: string;
}

interface Token {
  token: string;
}

interface Registered {
  agent_id: string;
}

interface Gate {
  url: string;
  open: () => void;
  close: () => Promise<void>;
}

const FIRST_PASSWORD = 'the first password of e2e';

const OWN_PASSWORD = 'a passphrase of my own choosing';

export const NO_TARGET = { attempt: null, job: null, step: null };

function ownerToken(): string {
  const token = process.env.BOSUN_E2E_TOKEN;
  if (token === undefined || token === '') {
    throw new Error('BOSUN_E2E_TOKEN holds no token of an owner; hack/e2e.sh sets it');
  }
  return token;
}

export function ownerHeaders(): Record<string, string> {
  return { Authorization: `Bearer ${ownerToken()}` };
}

export function unique(theme: string): string {
  return `web-${theme}-${randomBytes(4).toString('hex')}`;
}

export async function asOwner(playwright: PlaywrightWorkerArgs['playwright']): Promise<APIRequestContext> {
  return playwright.request.newContext({
    baseURL: process.env.BOSUN_E2E_URL,
    extraHTTPHeaders: { Authorization: `Bearer ${ownerToken()}` },
  });
}

export async function created<T>(answer: Promise<APIResponse>): Promise<T> {
  const response = await answer;
  const text = await response.text();
  if (!response.ok()) {
    throw new Error(`the server refused the request: ${text}`);
  }
  return JSON.parse(text) as T;
}

export async function project(owner: APIRequestContext, slug: string): Promise<string> {
  await created<Created>(owner.post('/api/v1/projects', { data: { organisation: 'e2e', slug, name: slug } }));
  return `e2e/${slug}`;
}

export async function pool(owner: APIRequestContext, name: string, path: string): Promise<string> {
  await created<Created>(
    owner.post('/api/v1/pools', {
      data: { name, trust: 'trusted', access: { all_projects: false, organisations: [], projects: [path] } },
    }),
  );
  return name;
}

export async function registered(owner: APIRequestContext, token: string, name: string): Promise<string> {
  const agent = await created<Registered>(
    owner.post('/api/v1/agents/register', { data: { token, name, labels: [] } }),
  );
  return agent.agent_id;
}

export async function agentIn(owner: APIRequestContext, chosen: string, name: string): Promise<string> {
  const { token } = await created<Token>(
    owner.post('/api/v1/registration-tokens', { data: { pool: chosen, reusable: false, expires_at: null } }),
  );
  return registered(owner, token, name);
}

export async function workflow(owner: APIRequestContext, path: string, source: string): Promise<Workflow> {
  const name = /^name: (.+)$/m.exec(source)?.[1] ?? 'workflow';
  return created<Workflow>(owner.post(`/api/v1/projects/${path}/workflows`, { data: { name, source } }));
}

export async function dispatch(owner: APIRequestContext, chosen: Workflow): Promise<string> {
  const run = await created<Created>(
    owner.post(`/api/v1/workflows/${chosen.id}/dispatch`, { data: { ref: null, inputs: {} } }),
  );
  return run.id;
}

export async function person(
  owner: APIRequestContext,
  login: string,
  role: string,
  path: string,
): Promise<Person> {
  await created<Created>(
    owner.post('/api/v1/users', {
      data: { login, name: login, email: `${login}@example.org`, password: FIRST_PASSWORD },
    }),
  );
  await created<Created>(
    owner.post('/api/v1/role-bindings', {
      data: { role, user: login, team: null, organisation: 'e2e', project: path },
    }),
  );
  return { login, password: FIRST_PASSWORD };
}

export async function signIn(page: Page, who: Person) {
  await page.goto('/sign-in');
  await page.getByLabel(english('signIn.login'), { exact: true }).fill(who.login);
  await page.getByLabel(english('signIn.password'), { exact: true }).fill(who.password);
  await page.getByRole('button', { name: english('signIn.submit'), exact: true }).click();
  await expect(page.getByRole('heading', { name: english('password.title') })).toBeVisible();
  await page.getByLabel(english('password.current'), { exact: true }).fill(who.password);
  await page.getByLabel(english('password.new'), { exact: true }).fill(OWN_PASSWORD);
  await page.getByLabel(english('password.repeat'), { exact: true }).fill(OWN_PASSWORD);
  await page.getByRole('button', { name: english('password.submit'), exact: true }).click();
  await expect(page.getByRole('heading', { name: english('home.title') })).toBeVisible();
}

export async function gate(): Promise<Gate> {
  let opened = false;
  const server = createServer((_, response) => {
    response.writeHead(opened ? 200 : 503).end();
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address() as AddressInfo;
  return {
    url: `http://127.0.0.1:${port}/`,
    open: () => {
      opened = true;
    },
    close: () => new Promise((resolve) => server.close(() => resolve())),
  };
}
