import { Box, Code, Flex, Grid, Heading, Link, Text } from '@radix-ui/themes';
import { useMemo } from 'react';
import { Link as RouterLink } from 'react-router';
import type { EventFilter, Project, QueuedJob, RunConclusion, RunFilters, Workflow } from '../api/client';
import { useProjects, useQueue, useWorkflowsOf } from '../api/queries';
import { useI18n } from '../i18n/i18n';
import { useLive } from '../live/live';
import { Runs } from '../runs/run-list';
import { StateBadge } from '../runs/state';
import { duration, useNow } from '../runs/time';
import { Section } from '../shell/section';
import { Failure, Loading } from '../shell/status';

interface ProjectsProps {
  projects: Project[];
}

interface Failing {
  project: Project;
  workflow: Workflow;
}

const RECENT = 15;

const NO_FILTERS: RunFilters = {};

const FAILING: ReadonlySet<RunConclusion> = new Set(['failure', 'timed_out', 'startup_failure']);

const HOME_EVENTS: EventFilter = { kind: ['run.created', 'run.status', 'queue.job'] };

function missingLabels(entries: QueuedJob[]): string[] {
  const labels = new Set<string>();
  for (const entry of entries) {
    for (const label of entry.missing_labels) {
      labels.add(label);
    }
  }
  return [...labels].sort();
}

function QueueSummary() {
  const { t } = useI18n();
  const queue = useQueue();
  const now = useNow((queue.data?.length ?? 0) > 0);
  if (queue.isPending) {
    return <Loading />;
  }
  if (queue.isError) {
    return <Failure error={queue.error} onRetry={() => queue.refetch()} />;
  }
  if (queue.data.length === 0) {
    return (
      <Text as="p" size="2" color="gray">
        {t('home.queue.empty')}
      </Text>
    );
  }
  const queued = queue.data.filter((entry) => entry.job.status === 'queued').length;
  const waiting = queue.data.length - queued;
  const since = queue.data
    .map((entry) => entry.since)
    .filter((at) => at !== null)
    .map((at) => Date.parse(at));
  const missing = missingLabels(queue.data);
  return (
    <Flex direction="column" gap="2">
      <Text size="2">{t('home.queue.queued', { count: queued })}</Text>
      <Text size="2">{t('home.queue.waiting', { count: waiting })}</Text>
      {since.length === 0 ? null : (
        <Text size="2">{t('home.queue.longest', { duration: duration(t, now - Math.min(...since)) })}</Text>
      )}
      {missing.length === 0 ? null : (
        <Text size="2" color="amber">
          {t('home.queue.missing', { labels: missing.join(', ') })}
        </Text>
      )}
    </Flex>
  );
}

function FailingWorkflows({ projects }: ProjectsProps) {
  const { t } = useI18n();
  const lists = useWorkflowsOf(projects);
  const failing: Failing[] = projects.flatMap((project, index) =>
    (lists[index]?.data ?? [])
      .filter((workflow) => {
        const conclusion = workflow.latest_run?.conclusion;
        return conclusion !== null && conclusion !== undefined && FAILING.has(conclusion);
      })
      .map((workflow) => ({ project, workflow })),
  );
  const broken = lists.find((list) => list.isError);
  if (broken?.error) {
    return <Failure error={broken.error} onRetry={() => broken.refetch()} />;
  }
  if (lists.some((list) => list.isPending)) {
    return <Loading />;
  }
  if (failing.length === 0) {
    return (
      <Text as="p" size="2" color="gray">
        {t('home.failing.empty')}
      </Text>
    );
  }
  return (
    <Flex asChild direction="column" gap="3" m="0" p="0">
      <ul className="plain-list">
        {failing.map(({ project, workflow }) => {
          const run = workflow.latest_run;
          return run === null ? null : (
            <li key={workflow.id}>
              <Flex direction="column" gap="1">
                <Flex align="center" gap="2" wrap="wrap">
                  <StateBadge status={run.status} conclusion={run.conclusion} />
                  <Link asChild size="2" weight="medium">
                    <RouterLink to={`/runs/${run.id}`}>
                      {workflow.name} #{run.number}
                    </RouterLink>
                  </Link>
                </Flex>
                <Text size="1" color="gray">
                  {project.path}
                </Text>
              </Flex>
            </li>
          );
        })}
      </ul>
    </Flex>
  );
}

function ProjectList({ projects }: ProjectsProps) {
  const { t } = useI18n();
  if (projects.length === 0) {
    return (
      <Text as="p" size="2" color="gray">
        {t('home.projects.empty')}
      </Text>
    );
  }
  return (
    <Flex asChild direction="column" gap="2" m="0" p="0">
      <ul className="plain-list">
        {projects.map((project) => (
          <li key={project.id}>
            <Flex direction="column">
              <Link asChild size="2" weight="medium">
                <RouterLink to={`/projects/${project.path}`}>{project.name}</RouterLink>
              </Link>
              <Code variant="ghost" size="1" color="gray">
                {project.path}
              </Code>
            </Flex>
          </li>
        ))}
      </ul>
    </Flex>
  );
}

export function HomePage() {
  const { t } = useI18n();
  useLive(HOME_EVENTS);
  const projects = useProjects();
  const byId = useMemo(
    () => new Map((projects.data ?? []).map((project) => [project.id, project])),
    [projects.data],
  );
  return (
    <Flex direction="column" gap="5">
      <Heading as="h1" size="7">
        {t('home.title')}
      </Heading>
      <Grid columns={{ initial: '1', md: '3' }} gap="5" align="start">
        <Box gridColumn={{ initial: '1', md: 'span 2' }}>
          <Section title={t('home.recent.title')}>
            {projects.isPending ? <Loading /> : null}
            {projects.isError ? <Failure error={projects.error} onRetry={() => projects.refetch()} /> : null}
            {projects.data === undefined ? null : (
              <Runs filters={NO_FILTERS} limit={RECENT} projects={byId} />
            )}
          </Section>
        </Box>
        <Flex direction="column" gap="5">
          <Section title={t('home.queue.title')}>
            <QueueSummary />
          </Section>
          <Section title={t('home.failing.title')}>
            {projects.data === undefined ? <Loading /> : <FailingWorkflows projects={projects.data} />}
          </Section>
          <Section title={t('home.projects.title')}>
            {projects.data === undefined ? <Loading /> : <ProjectList projects={projects.data} />}
          </Section>
        </Flex>
      </Grid>
    </Flex>
  );
}
