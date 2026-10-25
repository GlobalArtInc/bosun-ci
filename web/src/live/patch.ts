import type { QueryClient, QueryKey } from '@tanstack/react-query';
import type {
  Agent,
  Execution,
  Job,
  Notice,
  QueuedJob,
  Run,
  RunEvent,
  Step,
  StreamEvent,
  Workflow,
} from '../api/client';
import { AGENTS, executionKey, JOBS, QUEUE, RUNS, type RunPages, runKey, WORKFLOWS } from '../api/queries';

export type LiveEvent = RunEvent | StreamEvent;

type Of<K extends LiveEvent['kind']> = Extract<LiveEvent, { kind: K }>;

const STALE = Symbol('stale');

type Revision<T> = T | typeof STALE;

type Change<T> = (value: T) => Revision<T>;

interface Identified {
  id: string;
}

interface Moving<S> {
  status: S;
}

interface Transition<S> {
  from: S;
  to: S;
}

const WAITING_OR_QUEUED = new Set(['queued', 'waiting']);

function revise<T>(queryClient: QueryClient, key: QueryKey, change: Change<T>) {
  const data = queryClient.getQueryData<T>(key);
  if (data === undefined) {
    return;
  }
  const next = change(data);
  if (next === STALE) {
    void queryClient.invalidateQueries({ queryKey: key, exact: true });
  } else if (next !== data) {
    queryClient.setQueryData<T>(key, next);
  }
}

function reviseAll<T>(queryClient: QueryClient, prefix: QueryKey, change: Change<T>) {
  for (const [key] of queryClient.getQueriesData<T>({ queryKey: prefix })) {
    revise(queryClient, key, change);
  }
}

function moving<S, T extends Moving<S>>(transition: Transition<S>, move: (value: T) => T): Change<T> {
  return (value) => {
    if (value.status === transition.from) {
      return move(value);
    }
    return value.status === transition.to ? value : STALE;
  };
}

function within<T extends Identified>(id: string, change: Change<T>): Change<T[]> {
  return (items) => {
    const index = items.findIndex((item) => item.id === id);
    const item = items[index];
    if (item === undefined) {
      return items;
    }
    const next = change(item);
    if (next === STALE) {
      return STALE;
    }
    return next === item ? items : items.with(index, next);
  };
}

function inPages(id: string, change: Change<Run>): Change<RunPages> {
  return (data) => {
    let stale = false;
    let changed = false;
    const pages = data.pages.map((page) => {
      const items = within(id, change)(page.items);
      if (items === STALE) {
        stale = true;
        return page;
      }
      changed ||= items !== page.items;
      return items === page.items ? page : { ...page, items };
    });
    if (stale) {
      return STALE;
    }
    return changed ? { ...data, pages } : data;
  };
}

function latestRun(id: string, change: Change<Run>): Change<Workflow[]> {
  return (workflows) => {
    let stale = false;
    let changed = false;
    const next = workflows.map((workflow) => {
      if (workflow.latest_run?.id !== id) {
        return workflow;
      }
      const run = change(workflow.latest_run);
      if (run === STALE) {
        stale = true;
        return workflow;
      }
      changed ||= run !== workflow.latest_run;
      return { ...workflow, latest_run: run };
    });
    if (stale) {
      return STALE;
    }
    return changed ? next : workflows;
  };
}

function everyCopy(queryClient: QueryClient, id: string, change: Change<Run>) {
  revise(queryClient, runKey(id), change);
  reviseAll(queryClient, [RUNS], inPages(id, change));
  reviseAll(queryClient, [WORKFLOWS], latestRun(id, change));
}

function runStatus(queryClient: QueryClient, event: Of<'run.status'>) {
  const { to, conclusion } = event.data;
  everyCopy(
    queryClient,
    event.run_id,
    moving<Run['status'], Run>(event.data, (run) => ({
      ...run,
      status: to,
      conclusion,
      started_at: to === 'in_progress' ? (run.started_at ?? event.at) : run.started_at,
      completed_at: to === 'completed' ? event.at : run.completed_at,
    })),
  );
}

function sameNotice(left: Notice, right: Notice): boolean {
  return (
    left.job_id === right.job_id &&
    left.severity === right.severity &&
    left.code === right.code &&
    left.message === right.message &&
    left.file === right.file &&
    JSON.stringify(left.span) === JSON.stringify(right.span) &&
    JSON.stringify(left.path) === JSON.stringify(right.path)
  );
}

function runNotice(queryClient: QueryClient, event: Of<'run.notice'>) {
  const notice: Notice = { job_id: event.job_id, ...event.data };
  everyCopy(queryClient, event.run_id, (run) =>
    run.notices.some((known) => sameNotice(known, notice))
      ? run
      : { ...run, notices: [...run.notices, notice] },
  );
}

function jobCreated(queryClient: QueryClient, event: Of<'job.created'>) {
  const { job_id: id, attempt_id: attempt } = event;
  if (id === null || attempt === null) {
    return;
  }
  const job: Job = {
    id,
    run_id: event.run_id,
    attempt_id: attempt,
    key: event.data.key,
    name: event.data.name,
    group_id: event.data.group_id,
    matrix: event.data.matrix,
    needs: event.data.needs,
    labels: [],
    status: 'pending',
    waiting_reason: null,
    conclusion: null,
    conclusion_source: null,
    outputs: {},
    execution_id: null,
    queued_at: null,
    started_at: null,
    completed_at: null,
  };
  reviseAll<Job[]>(queryClient, [JOBS, event.run_id], (jobs) => {
    if (jobs.some((known) => known.id === id)) {
      return jobs;
    }
    if (jobs[0]?.attempt_id !== attempt) {
      return jobs.length === 0 ? STALE : jobs;
    }
    return [...jobs, job];
  });
}

function unknownIn<T extends Identified>(id: string, change: Change<T[]>): Change<T[]> {
  return (items) => (items.some((item) => item.id === id) ? change(items) : STALE);
}

function jobStatus(queryClient: QueryClient, event: Of<'job.status'>) {
  const { job_id: id } = event;
  if (id === null) {
    return;
  }
  const { from, to, waiting_reason, conclusion, conclusion_source } = event.data;
  const change = moving<Job['status'], Job>(event.data, (job) => ({
    ...job,
    status: to,
    waiting_reason,
    conclusion,
    conclusion_source,
    queued_at:
      to === 'queued' && from !== 'assigned'
        ? event.at
        : to === 'waiting' && from === 'pending'
          ? null
          : job.queued_at,
    started_at: to === 'in_progress' ? (job.started_at ?? event.at) : job.started_at,
    completed_at: to === 'completed' ? event.at : job.completed_at,
  }));
  reviseAll<Job[]>(queryClient, [JOBS, event.run_id], (jobs) => {
    if (jobs[0]?.attempt_id !== event.attempt_id) {
      return jobs;
    }
    return unknownIn(id, within(id, change))(jobs);
  });
}

function executionCreated(queryClient: QueryClient, event: Of<'execution.created'>) {
  const { execution_id: id, job_id: job } = event;
  if (id === null || job === null) {
    return;
  }
  if (queryClient.getQueryData(executionKey(id)) === undefined) {
    queryClient.setQueryData<Execution>(executionKey(id), {
      id,
      job_id: job,
      number: event.data.number,
      agent_id: event.data.agent_id,
      status: 'assigned',
      conclusion: null,
      reason: null,
      started_at: null,
      completed_at: null,
      steps: [],
    });
  }
  reviseAll<Job[]>(
    queryClient,
    [JOBS, event.run_id],
    within<Job>(job, (known) => (known.execution_id === id ? known : { ...known, execution_id: id })),
  );
  revise<Agent[]>(
    queryClient,
    AGENTS,
    within<Agent>(event.data.agent_id, (agent) =>
      agent.execution_id === id ? agent : { ...agent, execution_id: id },
    ),
  );
}

function executionStatus(queryClient: QueryClient, event: Of<'execution.status'>) {
  if (event.execution_id === null) {
    return;
  }
  const { to, conclusion, reason } = event.data;
  revise(
    queryClient,
    executionKey(event.execution_id),
    moving<Execution['status'], Execution>(event.data, (execution) => ({
      ...execution,
      status: to,
      conclusion,
      reason,
      started_at: to === 'in_progress' ? (execution.started_at ?? event.at) : execution.started_at,
      completed_at: to === 'completed' ? event.at : execution.completed_at,
    })),
  );
  if (to === 'completed') {
    const ended = event.execution_id;
    revise<Agent[]>(queryClient, AGENTS, (agents) =>
      agents.some((agent) => agent.execution_id === ended)
        ? agents.map((agent) => (agent.execution_id === ended ? { ...agent, execution_id: null } : agent))
        : agents,
    );
  }
}

function stepCreated(queryClient: QueryClient, event: Of<'step.created'>) {
  const { execution_id: execution, step_id: id } = event;
  if (execution === null || id === null) {
    return;
  }
  const step: Step = {
    id,
    execution_id: execution,
    index: event.data.index,
    name: event.data.name,
    status: 'pending',
    outcome: null,
    conclusion: null,
    conclusion_source: null,
    started_at: null,
    completed_at: null,
    annotations: [],
    summary_size: null,
  };
  revise<Execution>(queryClient, executionKey(execution), (known) =>
    known.steps.some((other) => other.id === id)
      ? known
      : { ...known, steps: [...known.steps, step].sort((left, right) => left.index - right.index) },
  );
}

function stepStatus(queryClient: QueryClient, event: Of<'step.status'>) {
  const { execution_id: execution, step_id: id } = event;
  if (execution === null || id === null) {
    return;
  }
  const { to, outcome, conclusion, conclusion_source } = event.data;
  const change = moving<Step['status'], Step>(event.data, (step) => ({
    ...step,
    status: to,
    outcome,
    conclusion,
    conclusion_source,
    started_at: to === 'in_progress' ? (step.started_at ?? event.at) : step.started_at,
    completed_at: to === 'completed' ? event.at : step.completed_at,
  }));
  revise<Execution>(queryClient, executionKey(execution), (known) => {
    const steps = unknownIn(id, within(id, change))(known.steps);
    if (steps === STALE) {
      return STALE;
    }
    return steps === known.steps ? known : { ...known, steps };
  });
}

function queueJob(queryClient: QueryClient, event: Of<'queue.job'>) {
  const entry: QueuedJob = event.data;
  revise<QueuedJob[]>(queryClient, QUEUE, (entries) => {
    const others = entries.filter((known) => known.job.id !== entry.job.id);
    if (!WAITING_OR_QUEUED.has(entry.job.status)) {
      return others.length === entries.length ? entries : others;
    }
    const index = entries.findIndex((known) => known.job.id === entry.job.id);
    return index === -1 ? [...entries, entry] : entries.with(index, entry);
  });
}

function agentStatus(queryClient: QueryClient, event: Of<'agent.status'>) {
  const agent: Agent = event.data;
  revise<Agent[]>(queryClient, AGENTS, (agents) => {
    const index = agents.findIndex((known) => known.id === agent.id);
    const known = agents[index];
    if (known === undefined || !agent.online) {
      return STALE;
    }
    return agents.with(index, agent);
  });
}

export function apply(queryClient: QueryClient, event: LiveEvent) {
  switch (event.kind) {
    case 'run.created':
      void queryClient.invalidateQueries({ queryKey: [RUNS] });
      void queryClient.invalidateQueries({ queryKey: [WORKFLOWS] });
      return;
    case 'attempt.created':
      void queryClient.invalidateQueries({ queryKey: runKey(event.run_id) });
      void queryClient.invalidateQueries({ queryKey: [JOBS, event.run_id] });
      return;
    case 'run.status':
      runStatus(queryClient, event);
      return;
    case 'run.notice':
      runNotice(queryClient, event);
      return;
    case 'job.created':
      jobCreated(queryClient, event);
      return;
    case 'job.status':
      jobStatus(queryClient, event);
      return;
    case 'execution.created':
      executionCreated(queryClient, event);
      return;
    case 'execution.status':
      executionStatus(queryClient, event);
      return;
    case 'step.created':
      stepCreated(queryClient, event);
      return;
    case 'step.status':
      stepStatus(queryClient, event);
      return;
    case 'queue.job':
      queueJob(queryClient, event);
      return;
    case 'agent.status':
      agentStatus(queryClient, event);
      return;
    default:
      return;
  }
}
