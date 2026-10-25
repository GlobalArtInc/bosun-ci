import { describe, expect, it } from 'vitest';
import invalidInput from '../../../contracts/fixtures/api/error-invalid-input.json';
import invalidState from '../../../contracts/fixtures/api/error-invalid-state.json';
import me from '../../../contracts/fixtures/api/me.json';
import passwordChange from '../../../contracts/fixtures/api/password-change-request.json';
import methods from '../../../contracts/fixtures/api/sign-in-methods.json';
import signIn from '../../../contracts/fixtures/api/sign-in-request.json';
import { Api, ApiError, CSRF_HEADER } from './client';

const ORIGIN = 'http://bosun.test';

interface Exchange {
  api: Api;
  requests: Request[];
  answer: (response: Response) => void;
}

function json(status: number, body: unknown, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json', ...headers },
  });
}

function exchange(): Exchange {
  const requests: Request[] = [];
  const answers: Response[] = [];
  const api = new Api({
    baseUrl: ORIGIN,
    fetch: async (request) => {
      requests.push(request);
      const answer = answers.shift();
      if (answer === undefined) {
        throw new Error(`no answer was prepared for ${request.method} ${request.url}`);
      }
      return answer;
    },
  });
  return { api, requests, answer: (response) => answers.push(response) };
}

async function refusal(promise: Promise<unknown>): Promise<ApiError> {
  try {
    await promise;
  } catch (error) {
    if (error instanceof ApiError) {
      return error;
    }
    throw error;
  }
  throw new Error('the request did not fail');
}

describe('the API client', () => {
  it('reads the caller and sends its CSRF token with every request that changes something', async () => {
    const { api, requests, answer } = exchange();
    answer(json(200, me));
    answer(json(200, me));
    answer(new Response(null, { status: 204 }));

    expect(await api.me()).toEqual(me);
    expect(await api.changePassword(passwordChange)).toEqual(me);
    await api.signOut();

    const [read, change, out] = requests;
    expect(read?.method).toBe('GET');
    expect(read?.url).toBe(`${ORIGIN}/api/v1/me`);
    expect(read?.headers.has(CSRF_HEADER)).toBe(false);
    expect(change?.url).toBe(`${ORIGIN}/api/v1/auth/password`);
    expect(change?.headers.get(CSRF_HEADER)).toBe(me.csrf_token);
    expect(change?.headers.get('Content-Type')).toBe('application/json');
    expect(await change?.json()).toEqual(passwordChange);
    expect(out?.url).toBe(`${ORIGIN}/api/v1/auth/logout`);
    expect(out?.headers.get(CSRF_HEADER)).toBe(me.csrf_token);
  });

  it('signs in with a JSON body and keeps the token of the new session', async () => {
    const { api, requests, answer } = exchange();
    answer(json(200, me));
    answer(new Response(null, { status: 204 }));
    answer(json(200, me));

    expect(await api.signIn(signIn)).toEqual(me);
    await api.signOut();
    await api.changePassword(passwordChange);

    const [login, out, change] = requests;
    expect(login?.method).toBe('POST');
    expect(login?.url).toBe(`${ORIGIN}/api/v1/auth/login`);
    expect(login?.headers.get('Content-Type')).toBe('application/json');
    expect(login?.headers.has(CSRF_HEADER)).toBe(false);
    expect(await login?.json()).toEqual(signIn);
    expect(out?.headers.get(CSRF_HEADER)).toBe(me.csrf_token);
    expect(change?.headers.has(CSRF_HEADER)).toBe(false);
  });

  it('reads the sign-in methods', async () => {
    const { api, requests, answer } = exchange();
    answer(json(200, methods));

    expect(await api.methods()).toEqual(methods);
    expect(requests[0]?.url).toBe(`${ORIGIN}/api/v1/auth/methods`);
  });

  it.each([
    [422, invalidInput],
    [409, invalidState],
  ])(
    'turns a %i with the error envelope into an error with its code, fields and diagnostics',
    async (status, envelope) => {
      const { api, answer } = exchange();
      answer(json(status, envelope));

      const error = await refusal(api.me());

      expect(error.status).toBe(status);
      expect(error.code).toBe(envelope.error.code);
      expect(error.message).toBe(envelope.error.message);
      expect(error.body).toEqual(envelope.error);
      expect(error.retryAfter).toBeNull();
    },
  );

  it('finds the field at fault and the time to wait', async () => {
    const { api, answer } = exchange();
    answer(json(422, invalidInput));
    answer(
      json(
        429,
        {
          error: {
            code: 'rate_limited',
            message: 'too many failed sign-ins for `admin`; sign-in is locked for 60 more seconds',
            fields: [],
            diagnostics: [],
          },
        },
        { 'Retry-After': '60' },
      ),
    );

    const invalid = await refusal(api.signIn(signIn));
    const locked = await refusal(api.signIn(signIn));

    expect(invalid.field('source')).toEqual(invalidInput.error.fields[0]);
    expect(invalid.field('login')).toBeUndefined();
    expect(locked.code).toBe('rate_limited');
    expect(locked.retryAfter).toBe(60);
  });

  it('turns an answer without the envelope into an internal error', async () => {
    const { api, answer } = exchange();
    answer(new Response('<html>Bad Gateway</html>', { status: 502, statusText: 'Bad Gateway' }));

    const error = await refusal(api.methods());

    expect(error.status).toBe(502);
    expect(error.code).toBe('internal');
    expect(error.body.fields).toEqual([]);
    expect(error.body.diagnostics).toEqual([]);
  });
});
