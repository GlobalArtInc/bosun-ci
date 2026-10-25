import type { Execution, Job, Notice, QueuedJob, Run, Workflow } from '../../api/client';
import type { LiveEvent } from '../../live/patch';

interface State {
  run: Run;
  jobs: Job[];
  executions: Execution[];
  queue: QueuedJob[];
  workflows: Workflow[];
}

function id(n: number): string {
  return `0192f3a4-5b6c-7d8e-9f01-${n.toString(16).padStart(12, '0')}`;
}

function at(second: number): string {
  return new Date(Date.UTC(2026, 9, 24, 18, 34, 50 + second)).toISOString();
}

export const PROJECT = id(0x102);
export const RUN = id(0x107);
const WORKFLOW = id(0x105);
const REVISION = id(0x106);
const ATTEMPT = id(0x108);
const BUILD = id(0x109);
const GROUP = id(0x10a);
const MEMBER = id(0x112);
const AGENT = id(0x10e);
const BUILT = id(0x10b);
const TESTED = id(0x113);
const COMPILE = id(0x10c);
const CHECK = id(0x114);

const SYSTEM = { kind: 'system' } as const;

const BY_AGENT = { kind: 'agent', id: AGENT, name: 'runner1-1' } as const;

const NOTICE: Notice = {
  job_id: MEMBER,
  severity: 'warning',
  code: 'deprecated_command',
  message:
    'the step of job `test (linux)` uses `::set-output`, which is deprecated; write to `$GITHUB_OUTPUT`',
  file: 'ci',
  span: { line: 14, column: 9, end_line: 14, end_column: 40 },
  path: ['jobs', 'test', 'steps', 0, 'run'],
};

const queuedRun: Run = {
  id: RUN,
  project_id: PROJECT,
  workflow_id: WORKFLOW,
  revision_id: REVISION,
  number: 12,
  global_number: 4031,
  display_name: 'ci',
  event: 'workflow_dispatch',
  ref: null,
  sha: null,
  status: 'queued',
  conclusion: null,
  attempt: 1,
  actor: { kind: 'user', id: id(0x103), login: 'roman' },
  notices: [],
  created_at: at(0),
  started_at: null,
  completed_at: null,
};

const job: Omit<Job, 'id' | 'key' | 'name' | 'needs' | 'status'> = {
  run_id: RUN,
  attempt_id: ATTEMPT,
  group_id: null,
  matrix: null,
  labels: ['self-hosted'],
  waiting_reason: null,
  conclusion: null,
  conclusion_source: null,
  outputs: {},
  execution_id: null,
  queued_at: null,
  started_at: null,
  completed_at: null,
};

const queuedBuild: Job = {
  ...job,
  id: BUILD,
  key: 'build',
  name: 'build',
  needs: [],
  status: 'queued',
  queued_at: at(0),
};

const pendingGroup: Job = {
  ...job,
  id: GROUP,
  key: 'test',
  name: 'test',
  needs: ['build'],
  status: 'pending',
};

const entry = { project_id: PROJECT, missing_labels: [] };

const workflow: Omit<Workflow, 'latest_run'> = {
  id: WORKFLOW,
  project_id: PROJECT,
  name: 'ci',
  source: { kind: 'server' },
  enabled: true,
  revision_id: REVISION,
  created_at: at(-60),
};

function base(seq: number, second: number) {
  return {
    seq,
    run_id: RUN,
    attempt_id: ATTEMPT,
    job_id: null,
    execution_id: null,
    step_id: null,
    actor: SYSTEM,
    at: at(second),
  };
}

export const initial: State = {
  run: queuedRun,
  jobs: [queuedBuild, pendingGroup],
  executions: [],
  queue: [{ ...entry, job: queuedBuild, since: at(0) }],
  workflows: [{ ...workflow, latest_run: queuedRun }],
};

export const events: LiveEvent[] = [
  {
    ...base(101, 1),
    job_id: BUILD,
    execution_id: BUILT,
    kind: 'execution.created',
    data: { number: 1, agent_id: AGENT },
  },
  {
    ...base(102, 1),
    job_id: BUILD,
    kind: 'job.status',
    data: {
      from: 'queued',
      to: 'assigned',
      waiting_reason: null,
      conclusion: null,
      conclusion_source: null,
      reason: null,
    },
  },
  {
    seq: 31,
    project_id: PROJECT,
    run_id: RUN,
    at: at(1),
    kind: 'queue.job',
    data: { ...entry, job: { ...queuedBuild, status: 'assigned', execution_id: BUILT }, since: null },
  },
  {
    ...base(103, 2),
    job_id: BUILD,
    execution_id: BUILT,
    kind: 'execution.status',
    data: { from: 'assigned', to: 'in_progress', conclusion: null, reason: null },
  },
  {
    ...base(104, 2),
    job_id: BUILD,
    kind: 'job.status',
    data: {
      from: 'assigned',
      to: 'in_progress',
      waiting_reason: null,
      conclusion: null,
      conclusion_source: null,
      reason: null,
    },
  },
  {
    ...base(105, 2),
    kind: 'run.status',
    data: { from: 'queued', to: 'in_progress', conclusion: null },
  },
  {
    ...base(106, 2),
    job_id: BUILD,
    execution_id: BUILT,
    step_id: COMPILE,
    actor: BY_AGENT,
    kind: 'step.created',
    data: { index: 0, name: 'Compile' },
  },
  {
    ...base(107, 3),
    job_id: BUILD,
    execution_id: BUILT,
    step_id: COMPILE,
    actor: BY_AGENT,
    kind: 'step.status',
    data: { from: 'pending', to: 'in_progress', outcome: null, conclusion: null, conclusion_source: null },
  },
  {
    ...base(108, 9),
    job_id: BUILD,
    execution_id: BUILT,
    step_id: COMPILE,
    actor: BY_AGENT,
    kind: 'step.status',
    data: {
      from: 'in_progress',
      to: 'completed',
      outcome: 'success',
      conclusion: 'success',
      conclusion_source: 'natural',
    },
  },
  {
    ...base(109, 10),
    job_id: BUILD,
    execution_id: BUILT,
    actor: BY_AGENT,
    kind: 'execution.status',
    data: { from: 'in_progress', to: 'completed', conclusion: 'success', reason: null },
  },
  {
    ...base(110, 10),
    job_id: BUILD,
    actor: BY_AGENT,
    kind: 'job.status',
    data: {
      from: 'in_progress',
      to: 'completed',
      waiting_reason: null,
      conclusion: 'success',
      conclusion_source: 'natural',
      reason: null,
    },
  },
  {
    ...base(111, 10),
    job_id: MEMBER,
    kind: 'job.created',
    data: { key: 'test', name: 'test (linux)', needs: ['build'], group_id: GROUP, matrix: { os: 'linux' } },
  },
  {
    ...base(112, 10),
    job_id: MEMBER,
    kind: 'job.status',
    data: {
      from: 'pending',
      to: 'queued',
      waiting_reason: null,
      conclusion: null,
      conclusion_source: null,
      reason: null,
    },
  },
  {
    ...base(113, 10),
    job_id: MEMBER,
    kind: 'run.notice',
    data: {
      severity: NOTICE.severity,
      code: NOTICE.code,
      message: NOTICE.message,
      file: NOTICE.file,
      span: NOTICE.span,
      path: NOTICE.path,
    },
  },
  {
    seq: 32,
    project_id: PROJECT,
    run_id: RUN,
    at: at(10),
    kind: 'queue.job',
    data: {
      ...entry,
      job: { ...job, id: MEMBER, key: 'test', name: 'test (linux)', needs: ['build'], status: 'queued' },
      since: at(10),
    },
  },
  {
    ...base(114, 11),
    job_id: MEMBER,
    execution_id: TESTED,
    kind: 'execution.created',
    data: { number: 1, agent_id: AGENT },
  },
  {
    ...base(115, 11),
    job_id: MEMBER,
    kind: 'job.status',
    data: {
      from: 'queued',
      to: 'assigned',
      waiting_reason: null,
      conclusion: null,
      conclusion_source: null,
      reason: null,
    },
  },
  {
    seq: 33,
    project_id: PROJECT,
    run_id: RUN,
    at: at(11),
    kind: 'queue.job',
    data: {
      ...entry,
      job: { ...job, id: MEMBER, key: 'test', name: 'test (linux)', needs: ['build'], status: 'assigned' },
      since: null,
    },
  },
  {
    ...base(116, 12),
    job_id: MEMBER,
    execution_id: TESTED,
    kind: 'execution.status',
    data: { from: 'assigned', to: 'in_progress', conclusion: null, reason: null },
  },
  {
    ...base(117, 12),
    job_id: MEMBER,
    kind: 'job.status',
    data: {
      from: 'assigned',
      to: 'in_progress',
      waiting_reason: null,
      conclusion: null,
      conclusion_source: null,
      reason: null,
    },
  },
  {
    ...base(118, 12),
    job_id: MEMBER,
    execution_id: TESTED,
    step_id: CHECK,
    actor: BY_AGENT,
    kind: 'step.created',
    data: { index: 0, name: 'Check' },
  },
  {
    ...base(119, 13),
    job_id: MEMBER,
    execution_id: TESTED,
    step_id: CHECK,
    actor: BY_AGENT,
    kind: 'step.status',
    data: { from: 'pending', to: 'in_progress', outcome: null, conclusion: null, conclusion_source: null },
  },
  {
    ...base(120, 20),
    job_id: MEMBER,
    execution_id: TESTED,
    step_id: CHECK,
    actor: BY_AGENT,
    kind: 'step.status',
    data: {
      from: 'in_progress',
      to: 'completed',
      outcome: 'failure',
      conclusion: 'failure',
      conclusion_source: 'natural',
    },
  },
  {
    ...base(121, 21),
    job_id: MEMBER,
    execution_id: TESTED,
    actor: BY_AGENT,
    kind: 'execution.status',
    data: { from: 'in_progress', to: 'completed', conclusion: 'failure', reason: null },
  },
  {
    ...base(122, 21),
    job_id: MEMBER,
    actor: BY_AGENT,
    kind: 'job.status',
    data: {
      from: 'in_progress',
      to: 'completed',
      waiting_reason: null,
      conclusion: 'failure',
      conclusion_source: 'natural',
      reason: null,
    },
  },
  {
    ...base(123, 21),
    job_id: GROUP,
    kind: 'job.status',
    data: {
      from: 'pending',
      to: 'completed',
      waiting_reason: null,
      conclusion: 'failure',
      conclusion_source: 'natural',
      reason: null,
    },
  },
  {
    ...base(124, 21),
    kind: 'run.status',
    data: { from: 'in_progress', to: 'completed', conclusion: 'failure' },
  },
];

const completedRun: Run = {
  ...queuedRun,
  status: 'completed',
  conclusion: 'failure',
  notices: [NOTICE],
  started_at: at(2),
  completed_at: at(21),
};

export const expected: State = {
  run: completedRun,
  jobs: [
    {
      ...queuedBuild,
      status: 'completed',
      conclusion: 'success',
      conclusion_source: 'natural',
      execution_id: BUILT,
      started_at: at(2),
      completed_at: at(10),
    },
    {
      ...pendingGroup,
      status: 'completed',
      conclusion: 'failure',
      conclusion_source: 'natural',
      completed_at: at(21),
    },
    {
      ...job,
      id: MEMBER,
      key: 'test',
      name: 'test (linux)',
      needs: ['build'],
      group_id: GROUP,
      matrix: { os: 'linux' },
      labels: [],
      status: 'completed',
      conclusion: 'failure',
      conclusion_source: 'natural',
      execution_id: TESTED,
      queued_at: at(10),
      started_at: at(12),
      completed_at: at(21),
    },
  ],
  executions: [
    {
      id: BUILT,
      job_id: BUILD,
      number: 1,
      agent_id: AGENT,
      status: 'completed',
      conclusion: 'success',
      reason: null,
      started_at: at(2),
      completed_at: at(10),
      steps: [
        {
          id: COMPILE,
          execution_id: BUILT,
          index: 0,
          name: 'Compile',
          status: 'completed',
          outcome: 'success',
          conclusion: 'success',
          conclusion_source: 'natural',
          started_at: at(3),
          completed_at: at(9),
          annotations: [],
          summary_size: null,
        },
      ],
    },
    {
      id: TESTED,
      job_id: MEMBER,
      number: 1,
      agent_id: AGENT,
      status: 'completed',
      conclusion: 'failure',
      reason: null,
      started_at: at(12),
      completed_at: at(21),
      steps: [
        {
          id: CHECK,
          execution_id: TESTED,
          index: 0,
          name: 'Check',
          status: 'completed',
          outcome: 'failure',
          conclusion: 'failure',
          conclusion_source: 'natural',
          started_at: at(13),
          completed_at: at(20),
          annotations: [],
          summary_size: null,
        },
      ],
    },
  ],
  queue: [],
  workflows: [{ ...workflow, latest_run: completedRun }],
};
