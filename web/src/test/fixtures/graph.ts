import type { Job, Me, Permission, Project, Run, RunEvent } from '../../api/client';

function id(n: number): string {
  return `0192f3a4-5b6c-7d8e-9f01-${n.toString(16).padStart(12, '0')}`;
}

function at(second: number): string {
  return new Date(Date.UTC(2026, 9, 24, 18, 34, 50 + second)).toISOString();
}

const PROJECT = id(0x202);
const ORGANISATION = id(0x201);
const ATTEMPT = id(0x208);
const FORKED_ATTEMPT = id(0x228);

const RUN = id(0x207);
export const FORKED = id(0x227);
export const BUILD = id(0x209);
export const TEST = id(0x20a);
export const LINUX_20 = id(0x210);
export const LINUX_22 = id(0x211);
export const MACOS_20 = id(0x212);
export const LINT = id(0x20b);
export const DEPLOY = id(0x20c);
const FORKED_BUILD = id(0x229);

const DEVELOPER: Permission[] = ['project.view', 'run.dispatch', 'run.cancel', 'run.rerun', 'run.control'];

const MAINTAINER: Permission[] = [
  ...DEVELOPER,
  'run.override',
  'job.debug',
  'workflow.edit',
  'workflow.commit',
  'workflow.manage',
  'secret.manage',
  'variable.manage',
  'environment.manage',
];

export const matrixRun: Run = {
  id: RUN,
  project_id: PROJECT,
  workflow_id: id(0x205),
  revision_id: id(0x206),
  number: 7,
  global_number: 5120,
  display_name: 'ci',
  event: 'push',
  ref: 'refs/heads/main',
  sha: '8f2d0c41a77b3e5c9d6f1a2b3c4d5e6f7a8b9c0d',
  status: 'in_progress',
  conclusion: null,
  attempt: 1,
  actor: { kind: 'user', id: id(0x203), login: 'roman' },
  notices: [],
  created_at: at(0),
  started_at: at(1),
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

const member: Omit<Job, 'id' | 'name' | 'matrix' | 'status'> = {
  ...job,
  key: 'test',
  needs: ['build'],
  group_id: TEST,
};

export const matrixJobs: Job[] = [
  {
    ...job,
    id: BUILD,
    key: 'build',
    name: 'build',
    needs: [],
    status: 'completed',
    conclusion: 'success',
    conclusion_source: 'natural',
    queued_at: at(0),
    started_at: at(1),
    completed_at: at(13),
  },
  { ...job, id: TEST, key: 'test', name: 'test', needs: ['build'], status: 'pending' },
  {
    ...member,
    id: LINUX_20,
    name: 'test (linux, 20)',
    matrix: { os: 'linux', node: 20 },
    status: 'completed',
    conclusion: 'success',
    conclusion_source: 'natural',
    queued_at: at(13),
    started_at: at(14),
    completed_at: at(30),
  },
  {
    ...member,
    id: LINUX_22,
    name: 'test (linux, 22)',
    matrix: { os: 'linux', node: 22 },
    status: 'in_progress',
    queued_at: at(13),
    started_at: at(15),
  },
  {
    ...member,
    id: MACOS_20,
    name: 'test (macos, 20)',
    matrix: { os: 'macos', node: 20 },
    status: 'waiting',
    waiting_reason: 'max_parallel',
  },
  {
    ...job,
    id: LINT,
    key: 'lint',
    name: 'lint',
    needs: ['build'],
    status: 'queued',
    queued_at: at(13),
  },
  { ...job, id: DEPLOY, key: 'deploy', name: 'deploy', needs: ['test', 'lint'], status: 'pending' },
];

export const forkedRun: Run = {
  ...matrixRun,
  id: FORKED,
  number: 8,
  global_number: 5121,
  event: 'pull_request',
  ref: 'refs/pull/41/merge',
  status: 'waiting',
  started_at: null,
};

export const forkedJobs: Job[] = [
  {
    ...job,
    id: FORKED_BUILD,
    run_id: FORKED,
    attempt_id: FORKED_ATTEMPT,
    key: 'build',
    name: 'build',
    needs: [],
    status: 'waiting',
    waiting_reason: 'untrusted',
  },
  {
    ...job,
    id: id(0x22a),
    run_id: FORKED,
    attempt_id: FORKED_ATTEMPT,
    key: 'test',
    name: 'test',
    needs: ['build'],
    status: 'pending',
  },
];

const lintEvent: RunEvent = {
  seq: 41,
  run_id: RUN,
  attempt_id: ATTEMPT,
  job_id: LINT,
  execution_id: null,
  step_id: null,
  actor: { kind: 'system' },
  at: at(16),
  kind: 'job.status',
  data: {
    from: 'queued',
    to: 'assigned',
    waiting_reason: null,
    conclusion: null,
    conclusion_source: null,
    reason: null,
  },
};

export const lintEvents: RunEvent[] = [
  lintEvent,
  {
    ...lintEvent,
    seq: 42,
    at: at(17),
    data: { ...lintEvent.data, from: 'assigned', to: 'in_progress' },
  },
  {
    ...lintEvent,
    seq: 43,
    at: at(25),
    data: {
      ...lintEvent.data,
      from: 'in_progress',
      to: 'completed',
      conclusion: 'failure',
      conclusion_source: 'natural',
    },
  },
];

function person(login: string, permissions: Permission[]): Me {
  return {
    user: {
      id: id(0x203),
      login,
      name: login,
      email: null,
      owner: false,
      created_at: at(-3600),
    },
    csrf_token: 'Wq3vN8sLk2JdXoBf2c7sTn9WdYpEh3aGuKqC5iV8xMr',
    password_change_required: false,
    permissions: { installation: [], organisations: {}, projects: { [PROJECT]: permissions } },
  };
}

export const maintainer = person('mira', MAINTAINER);

export const developer = person('dev', DEVELOPER);

export const project: Project = {
  id: PROJECT,
  organisation_id: ORGANISATION,
  slug: 'keel',
  path: 'globalartinc/keel',
  name: 'Keel',
  repository: null,
  created_at: at(-3600),
};
