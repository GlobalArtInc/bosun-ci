import createClient, { type Client, type Middleware } from 'openapi-fetch';
import type { components, paths } from '../../generated/api';

export type Me = components['schemas']['Me'];
type SignInMethods = components['schemas']['SignInMethods'];
export type SignInRequest = components['schemas']['SignInRequest'];
export type PasswordChangeRequest = components['schemas']['PasswordChangeRequest'];
type ErrorBody = components['schemas']['ErrorBody'];
type ErrorCode = components['schemas']['ErrorCode'];
type FieldError = components['schemas']['FieldError'];

export const CSRF_HEADER = 'X-Bosun-CSRF';

const SAFE_METHODS = new Set(['GET', 'HEAD', 'OPTIONS']);

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
