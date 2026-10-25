import createClient, { type Client, type Middleware } from 'openapi-fetch';
import type { components, paths } from '../../generated/api';

export type Me = components['schemas']['Me'];
type SignInMethods = components['schemas']['SignInMethods'];
export type SignInRequest = components['schemas']['SignInRequest'];
export type PasswordChangeRequest = components['schemas']['PasswordChangeRequest'];
type ErrorBody = components['schemas']['ErrorBody'];
type ErrorCode = components['schemas']['ErrorCode'];
type FieldError = components['schemas']['FieldError'];
export type Permission = components['schemas']['Permission'];
export type Project = components['schemas']['Project'];
export type Workflow = components['schemas']['Workflow'];
export type Run = components['schemas']['Run'];
export type Job = components['schemas']['Job'];
export type Execution = components['schemas']['Execution'];
export type Step = components['schemas']['Step'];
export type Notice = components['schemas']['Notice'];
export type Actor = components['schemas']['Actor'];
export type QueuedJob = components['schemas']['QueuedJob'];
export type RunEvent = components['schemas']['RunEvent'];
export type StreamEvent = components['schemas']['StreamEvent'];
export type CommandRequest = components['schemas']['CommandRequest'];
export type RunStatus = components['schemas']['RunStatus'];
export type RunConclusion = components['schemas']['RunConclusion'];
export type JobStatus = components['schemas']['JobStatus'];
export type StepStatus = components['schemas']['StepStatus'];
export type ConclusionSource = components['schemas']['ConclusionSource'];
export type WaitingReason = components['schemas']['WaitingReason'];
export type Severity = components['schemas']['Severity'];
export type Agent = components['schemas']['Agent'];
export type Pool = components['schemas']['Pool'];
export type PoolAccess = components['schemas']['PoolAccess'];
export type CreatePoolRequest = components['schemas']['CreatePoolRequest'];
export type TrustLevel = components['schemas']['TrustLevel'];
export type Capability = components['schemas']['Capability'];
export type LabelAlias = components['schemas']['LabelAlias'];
export type SetLabelAliasRequest = components['schemas']['SetLabelAliasRequest'];
export type RegistrationTokenRequest = components['schemas']['RegistrationTokenRequest'];
export type RegistrationTokenCreated = components['schemas']['RegistrationTokenCreated'];
export type Organisation = components['schemas']['Organisation'];
export type Secret = components['schemas']['Secret'];
export type Variable = components['schemas']['Variable'];
export type NamedValue = components['schemas']['SetSecretRequest'];

type RunQuery = NonNullable<paths['/api/v1/runs']['get']['parameters']['query']>;

export type RunFilters = Omit<RunQuery, 'limit' | 'cursor'>;

export type EventFilter = NonNullable<paths['/api/v1/events']['get']['parameters']['query']>;

type SecretQuery = NonNullable<paths['/api/v1/secrets']['get']['parameters']['query']>;

export type Scope = Pick<SecretQuery, 'organisation' | 'project'>;

export interface Page<T> {
  items: T[];
  next: string | null;
}

export const CSRF_HEADER = 'X-Bosun-CSRF';

const SAFE_METHODS = new Set(['GET', 'HEAD', 'OPTIONS']);

const WHOLE = 500;

interface ApiOptions {
  baseUrl: string;
  fetch?: (request: Request) => Promise<Response>;
}

interface Answer<T> {
  data?: T;
  error?: unknown;
  response: Response;
}

export class ApiError extends Error {
  readonly status: number;
  readonly body: ErrorBody;
  readonly retryAfter: number | null;

  constructor(status: number, body: ErrorBody, retryAfter: number | null) {
    super(body.message);
    this.name = 'ApiError';
    this.status = status;
    this.body = body;
    this.retryAfter = retryAfter;
  }

  get code(): ErrorCode {
    return this.body.code;
  }

  field(name: string): FieldError | undefined {
    return this.body.fields.find((fault) => fault.field === name);
  }
}

function isErrorBody(value: unknown): value is ErrorBody {
  if (typeof value !== 'object' || value === null) {
    return false;
  }
  const body = value as Record<string, unknown>;
  return (
    typeof body.code === 'string' &&
    typeof body.message === 'string' &&
    Array.isArray(body.fields) &&
    Array.isArray(body.diagnostics)
  );
}

function retryAfter(response: Response): number | null {
  const header = response.headers.get('Retry-After');
  if (header === null || !/^\d+$/.test(header.trim())) {
    return null;
  }
  return Number(header.trim());
}

function toApiError(response: Response, error: unknown): ApiError {
  const envelope =
    typeof error === 'object' && error !== null ? (error as Record<string, unknown>).error : null;
  if (isErrorBody(envelope)) {
    return new ApiError(response.status, envelope, retryAfter(response));
  }
  return new ApiError(
    response.status,
    {
      code: 'internal',
      message:
        `the server answered ${response.status} ${response.statusText} without the error envelope`.trim(),
      fields: [],
      diagnostics: [],
    },
    retryAfter(response),
  );
}

function settle<T>(answer: Answer<T>): T {
  if (answer.error !== undefined || !answer.response.ok) {
    throw toApiError(answer.response, answer.error);
  }
  return answer.data as T;
}

function paged<T>(answer: Answer<components['schemas']['Page']>): Page<T> {
  return settle(answer) as Page<T>;
}

async function whole<T>(read: (cursor: string | undefined) => Promise<Page<T>>): Promise<T[]> {
  const items: T[] = [];
  let cursor: string | undefined;
  for (;;) {
    const page = await read(cursor);
    items.push(...page.items);
    if (page.next === null) {
      return items;
    }
    cursor = page.next;
  }
}

function bySlugs(path: string, params: Record<string, unknown>): string {
  return path.replace('{project}', String(params.project).split('/').map(encodeURIComponent).join('/'));
}

export class Api {
  private readonly client: Client<paths>;
  private csrf: string | null = null;

  constructor(options: ApiOptions) {
    this.client = createClient<paths>({
      baseUrl: options.baseUrl,
      credentials: 'same-origin',
      ...(options.fetch === undefined ? {} : { fetch: options.fetch }),
    });
    this.client.use(this.csrfMiddleware());
  }

  async me(): Promise<Me> {
    return this.remember(settle(await this.client.GET('/api/v1/me')));
  }

  async methods(): Promise<SignInMethods> {
    return settle(await this.client.GET('/api/v1/auth/methods'));
  }

  async signIn(request: SignInRequest): Promise<Me> {
    return this.remember(settle(await this.client.POST('/api/v1/auth/login', { body: request })));
  }

  async changePassword(request: PasswordChangeRequest): Promise<Me> {
    return this.remember(settle(await this.client.POST('/api/v1/auth/password', { body: request })));
  }

  async signOut(): Promise<void> {
    settle(await this.client.POST('/api/v1/auth/logout'));
    this.csrf = null;
  }

  async projects(): Promise<Project[]> {
    return whole(async (cursor) =>
      paged<Project>(
        await this.client.GET('/api/v1/projects', { params: { query: { limit: WHOLE, cursor } } }),
      ),
    );
  }

  async project(id: string): Promise<Project> {
    return settle(await this.client.GET('/api/v1/projects/{project}', { params: { path: { project: id } } }));
  }

  async projectAt(organisation: string, slug: string): Promise<Project> {
    return settle(
      await this.client.GET('/api/v1/projects/{project}', {
        params: { path: { project: `${organisation}/${slug}` } },
        pathSerializer: bySlugs,
      }),
    );
  }

  async workflows(project: string): Promise<Workflow[]> {
    return whole(async (cursor) =>
      paged<Workflow>(
        await this.client.GET('/api/v1/projects/{project}/workflows', {
          params: { path: { project }, query: { limit: WHOLE, cursor } },
        }),
      ),
    );
  }

  async runs(filters: RunFilters, limit: number, cursor: string | null): Promise<Page<Run>> {
    return paged<Run>(
      await this.client.GET('/api/v1/runs', {
        params: { query: { ...filters, limit, cursor: cursor ?? undefined } },
      }),
    );
  }

  async run(id: string): Promise<Run> {
    return settle(await this.client.GET('/api/v1/runs/{run}', { params: { path: { run: id } } }));
  }

  async jobs(run: string, attempt: number): Promise<Job[]> {
    return whole(async (cursor) =>
      paged<Job>(
        await this.client.GET('/api/v1/runs/{run}/attempts/{attempt}/jobs', {
          params: { path: { run, attempt }, query: { limit: WHOLE, cursor } },
        }),
      ),
    );
  }

  async execution(id: string): Promise<Execution> {
    return settle(
      await this.client.GET('/api/v1/executions/{execution}', { params: { path: { execution: id } } }),
    );
  }

  async job(id: string): Promise<Job> {
    return settle(await this.client.GET('/api/v1/jobs/{job}', { params: { path: { job: id } } }));
  }

  async queue(): Promise<QueuedJob[]> {
    return whole(async (cursor) =>
      paged<QueuedJob>(
        await this.client.GET('/api/v1/queue', { params: { query: { limit: WHOLE, cursor } } }),
      ),
    );
  }

  async agents(): Promise<Agent[]> {
    return whole(async (cursor) =>
      paged<Agent>(await this.client.GET('/api/v1/agents', { params: { query: { limit: WHOLE, cursor } } })),
    );
  }

  async drain(agent: string, drain: boolean): Promise<Agent> {
    return settle(
      await this.client.PATCH('/api/v1/agents/{agent}', {
        params: { path: { agent } },
        body: { drain },
      }),
    );
  }

  async deleteAgent(agent: string): Promise<void> {
    settle(await this.client.DELETE('/api/v1/agents/{agent}', { params: { path: { agent } } }));
  }

  async pools(): Promise<Pool[]> {
    return whole(async (cursor) =>
      paged<Pool>(await this.client.GET('/api/v1/pools', { params: { query: { limit: WHOLE, cursor } } })),
    );
  }

  async createPool(request: CreatePoolRequest, key: string): Promise<Pool> {
    return settle(
      await this.client.POST('/api/v1/pools', {
        params: { header: { 'Idempotency-Key': key } },
        body: request,
      }),
    );
  }

  async aliases(): Promise<LabelAlias[]> {
    return whole(async (cursor) =>
      paged<LabelAlias>(
        await this.client.GET('/api/v1/label-aliases', { params: { query: { limit: WHOLE, cursor } } }),
      ),
    );
  }

  async setAlias(request: SetLabelAliasRequest): Promise<LabelAlias> {
    return settle(await this.client.POST('/api/v1/label-aliases', { body: request }));
  }

  async registrationToken(request: RegistrationTokenRequest): Promise<RegistrationTokenCreated> {
    return settle(await this.client.POST('/api/v1/registration-tokens', { body: request }));
  }

  async organisation(reference: string): Promise<Organisation> {
    return settle(
      await this.client.GET('/api/v1/organisations/{organisation}', {
        params: { path: { organisation: reference } },
      }),
    );
  }

  async secrets(scope: Scope): Promise<Secret[]> {
    return whole(async (cursor) =>
      paged<Secret>(
        await this.client.GET('/api/v1/secrets', { params: { query: { ...scope, limit: WHOLE, cursor } } }),
      ),
    );
  }

  async setSecret(scope: Scope, request: NamedValue): Promise<Secret> {
    return settle(await this.client.POST('/api/v1/secrets', { params: { query: scope }, body: request }));
  }

  async deleteSecret(scope: Scope, name: string): Promise<void> {
    settle(await this.client.DELETE('/api/v1/secrets/{name}', { params: { path: { name }, query: scope } }));
  }

  async variables(scope: Scope): Promise<Variable[]> {
    return whole(async (cursor) =>
      paged<Variable>(
        await this.client.GET('/api/v1/variables', { params: { query: { ...scope, limit: WHOLE, cursor } } }),
      ),
    );
  }

  async setVariable(scope: Scope, request: NamedValue): Promise<Variable> {
    return settle(await this.client.POST('/api/v1/variables', { params: { query: scope }, body: request }));
  }

  async deleteVariable(scope: Scope, name: string): Promise<void> {
    settle(
      await this.client.DELETE('/api/v1/variables/{name}', { params: { path: { name }, query: scope } }),
    );
  }

  async command(run: string, command: CommandRequest, key: string): Promise<RunEvent> {
    return settle(
      await this.client.POST('/api/v1/runs/{run}/commands', {
        params: { path: { run }, header: { 'Idempotency-Key': key } },
        body: command,
      }),
    );
  }

  async events(
    filter: EventFilter,
    last: string | null,
    signal: AbortSignal,
  ): Promise<ReadableStream<Uint8Array>> {
    const body = settle(
      await this.client.GET('/api/v1/events', {
        params: { query: filter, header: last === null ? {} : { 'Last-Event-ID': last } },
        parseAs: 'stream',
        signal,
      }),
    );
    if (body === null) {
      throw new Error('the event stream has no body');
    }
    return body;
  }

  private remember(me: Me): Me {
    this.csrf = me.csrf_token;
    return me;
  }

  private csrfMiddleware(): Middleware {
    return {
      onRequest: ({ request }) => {
        if (this.csrf !== null && !SAFE_METHODS.has(request.method)) {
          request.headers.set(CSRF_HEADER, this.csrf);
        }
        return request;
      },
    };
  }
}
