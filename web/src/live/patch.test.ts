import { QueryClient } from '@tanstack/react-query';
import { describe, expect, it } from 'vitest';
import { Api } from '../api/client';
import { executionKey, jobsKey, QUEUE, type RunPages, runKey, runsKey, workflowsKey } from '../api/queries';
import { events, expected, initial, PROJECT, RUN } from '../test/fixtures/stream';
import { envelope, ORIGIN } from '../test/render';
import { stream } from './live';

const RECENT = runsKey({}, 15);

function position(index: number): string {
  return `${100 + index}.${100 + index}.${30 + index}.${30 + index}`;
}

function wire(from: number, to: number): string {
  const messages = events.slice(from, to).map((event, offset) => {
    return `id: ${position(from + offset)}\ndata: ${JSON.stringify(event)}\n\n`;
  });
  return `id: ${position(from - 1)}\nretry: 1\n\n: kept open\n\n${messages.join('')}`;
}

function invalidated(queryClient: QueryClient) {
  return queryClient
    .getQueryCache()
    .findAll({ predicate: (query) => query.state.isInvalidated })
    .map((query) => query.queryKey);
}

function cache(): QueryClient {
  const queryClient = new QueryClient();
  queryClient.setQueryData(runKey(RUN), initial.run);
  queryClient.setQueryData(jobsKey(RUN, 1), initial.jobs);
  queryClient.setQueryData(QUEUE, initial.queue);
  queryClient.setQueryData(workflowsKey(PROJECT), initial.workflows);
  queryClient.setQueryData<RunPages>(RECENT, {
    pages: [{ items: [initial.run], next: null }],
    pageParams: [null],
  });
  return queryClient;
}

async function follow(queryClient: QueryClient, bodies: string[]): Promise<Request[]> {
  const requests: Request[] = [];
  const api = new Api({
    baseUrl: ORIGIN,
    fetch: async (request) => {
      requests.push(request);
      const body = bodies.shift();
      return body === undefined
        ? envelope(422, 'invalid_input', 'the test stream has ended')
        : new Response(body, { headers: { 'Content-Type': 'text/event-stream' } });
    },
  });
  await stream(api, queryClient, { run: [RUN] }, { last: null }, new AbortController().signal);
  return requests;
}

function state(queryClient: QueryClient) {
  return {
    run: queryClient.getQueryData(runKey(RUN)),
    jobs: queryClient.getQueryData(jobsKey(RUN, 1)),
    executions: expected.executions.map((execution) => queryClient.getQueryData(executionKey(execution.id))),
    queue: queryClient.getQueryData(QUEUE),
    workflows: queryClient.getQueryData(workflowsKey(PROJECT)),
    recent: queryClient.getQueryData<RunPages>(RECENT)?.pages[0]?.items,
  };
}

describe('the event stream', () => {
  it('patches the run, its jobs, executions and steps, the queue and the lists to the expected state', async () => {
    const queryClient = cache();

    await follow(queryClient, [wire(0, events.length)]);

    expect(state(queryClient)).toEqual({ ...expected, recent: [expected.run] });
    expect(invalidated(queryClient)).toEqual([]);
  });

  it('reconnects with Last-Event-ID, and an event delivered twice changes nothing', async () => {
    const queryClient = cache();

    const requests = await follow(queryClient, [wire(0, 12), wire(11, events.length)]);

    expect(state(queryClient)).toEqual({ ...expected, recent: [expected.run] });
    expect(invalidated(queryClient)).toEqual([]);
    expect(requests[0]?.headers.has('Last-Event-ID')).toBe(false);
    expect(requests[1]?.headers.get('Last-Event-ID')).toBe(position(11));
    expect(new URL(requests[1]?.url ?? '').searchParams.getAll('run')).toEqual([RUN]);
  });

  it('refetches an object whose cached status the event does not start from', async () => {
    const queryClient = cache();
    queryClient.setQueryData(runKey(RUN), { ...initial.run, status: 'waiting' });

    await follow(queryClient, [wire(0, 6)]);

    expect(invalidated(queryClient)).toEqual([runKey(RUN)]);
  });
});
