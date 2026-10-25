import { QueryClient } from '@tanstack/react-query';
import { describe, expect, it } from 'vitest';
import agent from '../../../contracts/fixtures/api/agent.json';
import offline from '../../../contracts/fixtures/events/agent-status.json';
import created from '../../../contracts/fixtures/events/execution-created.json';
import ended from '../../../contracts/fixtures/events/execution-status.json';
import type { Agent } from '../api/client';
import { AGENTS } from '../api/queries';
import { apply, type LiveEvent } from './patch';

const busy = agent as Agent;

const idle: Agent = {
  ...busy,
  id: '0192f3a4-5b6c-7d8e-9f01-000000000020',
  name: 'runner1-2',
  execution_id: null,
};

const other: Agent = {
  ...busy,
  id: '0192f3a4-5b6c-7d8e-9f01-000000000021',
  name: 'runner2-1',
  execution_id: null,
};

function cache(agents: Agent[]): QueryClient {
  const queryClient = new QueryClient();
  queryClient.setQueryData(AGENTS, agents);
  return queryClient;
}

function invalidated(queryClient: QueryClient): boolean {
  return queryClient.getQueryState(AGENTS)?.isInvalidated ?? false;
}

function status(changed: Agent): LiveEvent {
  return { ...offline, kind: 'agent.status', data: changed } as LiveEvent;
}

describe('the cached agents', () => {
  it('take the agent an agent.status event carries', () => {
    const queryClient = cache([busy, idle]);

    apply(queryClient, status({ ...idle, drain: true }));

    expect(queryClient.getQueryData(AGENTS)).toEqual([busy, { ...idle, drain: true }]);
    expect(invalidated(queryClient)).toBe(false);
  });

  it('are read again for an agent they do not hold and for one that went offline', () => {
    const registered = cache([busy]);
    apply(registered, status(other));
    expect(invalidated(registered)).toBe(true);

    const gone = cache([{ ...busy, id: offline.data.id, online: true }]);
    apply(gone, offline as LiveEvent);
    expect(invalidated(gone)).toBe(true);
  });

  it('follow the execution of an agent from its creation to its end', () => {
    const queryClient = cache([{ ...busy, execution_id: null }, idle]);

    apply(queryClient, created as LiveEvent);
    expect(queryClient.getQueryData(AGENTS)).toEqual([busy, idle]);

    apply(queryClient, ended as LiveEvent);
    expect(queryClient.getQueryData(AGENTS)).toEqual([{ ...busy, execution_id: null }, idle]);
    expect(invalidated(queryClient)).toBe(false);
  });
});
