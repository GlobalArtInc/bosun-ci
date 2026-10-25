import { type InfiniteData, useInfiniteQuery, useQueries, useQuery } from '@tanstack/react-query';
import { useLiveReady } from '../live/context';
import type { Page, Project, Run, RunFilters, Scope } from './client';
import { useApi } from './context';

export type RunPages = InfiniteData<Page<Run>, string | null>;

const PROJECTS = ['projects'] as const;

export const RUNS = 'runs';

const RUN = 'run';

export const JOBS = 'jobs';

const EXECUTION = 'execution';

export const WORKFLOWS = 'workflows';

export const QUEUE = ['queue'] as const;

const PROJECT = 'project';

const PROJECT_AT = 'projectAt';

const QUEUE_REFRESH_MS = 30_000;

export const AGENTS = ['agents'] as const;

export const POOLS = ['pools'] as const;

export const ALIASES = ['aliases'] as const;

const JOB = 'job';

const ORGANISATION = 'organisation';

const SECRETS = 'secrets';

const SUMMARY = 'summary';

const VARIABLES = 'variables';

export function runsKey(filters: RunFilters, limit: number) {
  return [RUNS, filters, limit] as const;
}

export function runKey(id: string) {
  return [RUN, id] as const;
}

export function jobsKey(run: string, attempt: number) {
  return [JOBS, run, attempt] as const;
}

export function executionKey(id: string) {
  return [EXECUTION, id] as const;
}

export function workflowsKey(project: string) {
  return [WORKFLOWS, project] as const;
}

export function secretsKey(scope: Scope) {
  return [SECRETS, scope] as const;
}

export function variablesKey(scope: Scope) {
  return [VARIABLES, scope] as const;
}

export function useProjects() {
  const api = useApi();
  return useQuery({ queryKey: PROJECTS, queryFn: () => api.projects() });
}

export function useProject(id: string) {
  const api = useApi();
  return useQuery({ queryKey: [PROJECT, id], queryFn: () => api.project(id) });
}

export function useProjectAt(organisation: string, slug: string) {
  const api = useApi();
  return useQuery({
    queryKey: [PROJECT_AT, organisation, slug],
    queryFn: () => api.projectAt(organisation, slug),
  });
}

export function useWorkflows(project: string) {
  const api = useApi();
  const ready = useLiveReady();
  return useQuery({ queryKey: workflowsKey(project), queryFn: () => api.workflows(project), enabled: ready });
}

export function useWorkflowsOf(projects: Project[]) {
  const api = useApi();
  const ready = useLiveReady();
  return useQueries({
    queries: projects.map((project) => ({
      queryKey: workflowsKey(project.id),
      queryFn: () => api.workflows(project.id),
      enabled: ready,
    })),
  });
}

export function useRuns(filters: RunFilters, limit: number) {
  const api = useApi();
  const ready = useLiveReady();
  return useInfiniteQuery({
    queryKey: runsKey(filters, limit),
    queryFn: ({ pageParam }) => api.runs(filters, limit, pageParam),
    initialPageParam: null as string | null,
    getNextPageParam: (page) => page.next,
    enabled: ready,
  });
}

export function useRun(id: string) {
  const api = useApi();
  const ready = useLiveReady();
  return useQuery({ queryKey: runKey(id), queryFn: () => api.run(id), enabled: ready });
}

export function useJobs(run: string, attempt: number) {
  const api = useApi();
  const ready = useLiveReady();
  return useQuery({ queryKey: jobsKey(run, attempt), queryFn: () => api.jobs(run, attempt), enabled: ready });
}

export function useExecution(id: string | null) {
  const api = useApi();
  const ready = useLiveReady();
  return useQuery({
    queryKey: executionKey(id ?? ''),
    queryFn: () => api.execution(id ?? ''),
    enabled: ready && id !== null,
  });
}

export function useSummary(execution: string, step: string) {
  const api = useApi();
  return useQuery({
    queryKey: [SUMMARY, execution, step],
    queryFn: () => api.summary(execution, step),
    staleTime: Number.POSITIVE_INFINITY,
  });
}

export function useQueue() {
  const api = useApi();
  const ready = useLiveReady();
  return useQuery({
    queryKey: QUEUE,
    queryFn: () => api.queue(),
    refetchInterval: QUEUE_REFRESH_MS,
    enabled: ready,
  });
}

export function useJob(id: string | null) {
  const api = useApi();
  const ready = useLiveReady();
  return useQuery({
    queryKey: [JOB, id ?? ''],
    queryFn: () => api.job(id ?? ''),
    enabled: ready && id !== null,
  });
}

export function useAgents() {
  const api = useApi();
  const ready = useLiveReady();
  return useQuery({
    queryKey: AGENTS,
    queryFn: () => api.agents(),
    refetchInterval: QUEUE_REFRESH_MS,
    enabled: ready,
  });
}

export function usePools() {
  const api = useApi();
  return useQuery({ queryKey: POOLS, queryFn: () => api.pools() });
}

export function useAliases() {
  const api = useApi();
  return useQuery({ queryKey: ALIASES, queryFn: () => api.aliases() });
}

export function useOrganisation(reference: string) {
  const api = useApi();
  return useQuery({ queryKey: [ORGANISATION, reference], queryFn: () => api.organisation(reference) });
}

export function useOrganisations(ids: string[]) {
  const api = useApi();
  return useQueries({
    queries: ids.map((id) => ({ queryKey: [ORGANISATION, id], queryFn: () => api.organisation(id) })),
  });
}

export function useSecrets(scope: Scope) {
  const api = useApi();
  return useQuery({ queryKey: secretsKey(scope), queryFn: () => api.secrets(scope) });
}

export function useVariables(scope: Scope) {
  const api = useApi();
  return useQuery({ queryKey: variablesKey(scope), queryFn: () => api.variables(scope) });
}
