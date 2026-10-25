import { Badge, Box, Code, DataList, Flex, Grid, Heading, Link, Text } from '@radix-ui/themes';
import { ChevronRight, GitCommitHorizontal } from 'lucide-react';
import { Link as RouterLink, useParams, useSearchParams } from 'react-router';
import {
  ApiError,
  type ConclusionSource,
  type Job,
  type Notice,
  type Permission,
  type Run,
  type Severity,
  type Step,
} from '../api/client';
import { useExecution, useJobs, useProject, useRun } from '../api/queries';
import { type Key, useI18n } from '../i18n/i18n';
import { useLive } from '../live/live';
import { Cancel } from '../runs/cancel';
import { actorName, shortSha } from '../runs/labels';
import { RunTrigger } from '../runs/run-list';
import { StateBadge, WAITING } from '../runs/state';
import { nest } from '../runs/steps';
import { duration, span, stamp, useNow } from '../runs/time';
import { permissionsOn } from '../session/permissions';
import { useMe } from '../session/session';
import { Section } from '../shell/section';
import { Failure, Loading } from '../shell/status';

interface RunViewProps {
  run: Run;
}

interface HeaderProps {
  run: Run;
  jobs: Job[];
  allowed: Set<Permission>;
}

interface NoticesProps {
  notices: Notice[];
  jobs: Job[];
}

interface JobsProps {
  jobs: Job[];
  selected: string | null;
}

interface JobItemProps {
  job: Job;
  members: Job[];
  selected: string | null;
}

interface JobPanelProps {
  run: Run;
  job: Job;
  allowed: Set<Permission>;
}

interface StepItemProps {
  step: Step;
  now: number;
}

interface SeverityLook {
  key: Key;
  color: 'red' | 'amber' | 'blue';
}

const SEVERITY_ORDER: Record<Severity, number> = { error: 0, warning: 1, notice: 2 };

const SEVERITIES: Record<Severity, SeverityLook> = {
  error: { key: 'runPage.notices.error', color: 'red' },
  warning: { key: 'runPage.notices.warning', color: 'amber' },
  notice: { key: 'runPage.notices.notice', color: 'blue' },
};

function bySeverity(notices: Notice[]): Notice[] {
  return notices
    .map((notice, order) => ({ notice, order }))
    .sort(
      (left, right) =>
        SEVERITY_ORDER[left.notice.severity] - SEVERITY_ORDER[right.notice.severity] ||
        left.order - right.order,
    )
    .map(({ notice }) => notice);
}

function noticeKey(notice: Notice): string {
  const { severity, code, job_id, file, span, message } = notice;
  return [severity, code, job_id, file, span.line, span.column, message].join('|');
}

function runSource(run: Run, jobs: Job[]): ConclusionSource | null {
  if (run.conclusion === null) {
    return null;
  }
  return jobs.some((job) => job.conclusion_source === 'operator') ? 'operator' : 'natural';
}

function Header({ run, jobs, allowed }: HeaderProps) {
  const { t, language } = useI18n();
  const now = useNow(run.status !== 'completed');
  const took = span(run.started_at, run.completed_at, now);
  return (
    <Flex asChild direction="column" gap="4">
      <section aria-labelledby="run-title">
        <Flex align="center" justify="between" gap="4" wrap="wrap">
          <Flex align="center" gap="3" wrap="wrap">
            <Heading as="h1" size="7" id="run-title">
              {run.display_name} #{run.number}
            </Heading>
            <StateBadge status={run.status} conclusion={run.conclusion} source={runSource(run, jobs)} />
          </Flex>
          {run.status !== 'completed' && allowed.has('run.cancel') ? <Cancel run={run} /> : null}
        </Flex>
        <DataList.Root orientation={{ initial: 'vertical', sm: 'horizontal' }} size="2">
          <DataList.Item>
            <DataList.Label>{t('runPage.trigger')}</DataList.Label>
            <DataList.Value>
              <RunTrigger run={run} />
            </DataList.Value>
          </DataList.Item>
          <DataList.Item>
            <DataList.Label>{t('runPage.commit')}</DataList.Label>
            <DataList.Value>
              {run.sha === null ? (
                <Text color="gray">{t('runPage.noCommit')}</Text>
              ) : (
                <Flex as="span" align="center" gap="1">
                  <GitCommitHorizontal size={14} aria-hidden />
                  <Code variant="ghost" title={run.sha}>
                    {shortSha(run.sha)}
                  </Code>
                </Flex>
              )}
            </DataList.Value>
          </DataList.Item>
          <DataList.Item>
            <DataList.Label>{t('runPage.actor')}</DataList.Label>
            <DataList.Value>{actorName(run.actor)}</DataList.Value>
          </DataList.Item>
          <DataList.Item>
            <DataList.Label>{t('runPage.attempt')}</DataList.Label>
            <DataList.Value>{run.attempt}</DataList.Value>
          </DataList.Item>
          <DataList.Item>
            <DataList.Label>{t('runPage.created')}</DataList.Label>
            <DataList.Value>{stamp(language, run.created_at)}</DataList.Value>
          </DataList.Item>
          <DataList.Item>
            <DataList.Label>{t('runPage.duration')}</DataList.Label>
            <DataList.Value>{took === null ? '—' : duration(t, took)}</DataList.Value>
          </DataList.Item>
        </DataList.Root>
      </section>
    </Flex>
  );
}

function Notices({ notices, jobs }: NoticesProps) {
  const { t } = useI18n();
  return (
    <Flex asChild direction="column" gap="3" m="0" p="0">
      <ol className="plain-list" aria-label={t('runPage.notices.title')}>
        {bySeverity(notices).map((notice) => {
          const severity = SEVERITIES[notice.severity];
          const job = jobs.find((known) => known.id === notice.job_id);
          return (
            <li key={noticeKey(notice)}>
              <Flex direction="column" gap="1">
                <Flex align="center" gap="2" wrap="wrap">
                  <Badge color={severity.color} size="2" highContrast>
                    {t(severity.key)}
                  </Badge>
                  <Code variant="ghost" size="2">
                    {`${notice.file}:${notice.span.line}:${notice.span.column}`}
                  </Code>
                  {job === undefined ? null : (
                    <Text size="2" color="gray">
                      {t('runPage.notices.job', { name: job.name })}
                    </Text>
                  )}
                </Flex>
                <Text size="2">{notice.message}</Text>
              </Flex>
            </li>
          );
        })}
      </ol>
    </Flex>
  );
}

function JobItem({ job, members, selected }: JobItemProps) {
  const { t } = useI18n();
  const now = useNow(job.status !== 'completed');
  const took = span(job.started_at, job.completed_at, now);
  return (
    <li>
      <Flex direction="column" gap="1" py="1">
        <Flex align="center" justify="between" gap="3" wrap="wrap">
          <Link asChild size="2" weight="medium">
            <RouterLink
              to={`?job=${job.id}`}
              className="job-link"
              aria-current={selected === job.id ? 'true' : undefined}
            >
              {job.name}
            </RouterLink>
          </Link>
          <Flex align="center" gap="3">
            {took === null ? null : <Text size="1">{duration(t, took)}</Text>}
            <StateBadge status={job.status} conclusion={job.conclusion} source={job.conclusion_source} />
          </Flex>
        </Flex>
        {job.status === 'waiting' && job.waiting_reason !== null ? (
          <Text size="1" color="amber">
            {t(WAITING[job.waiting_reason])}
          </Text>
        ) : null}
        {members.length === 0 ? null : (
          <Box asChild pl="4" m="0">
            <ul className="plain-list">
              {members.map((member) => (
                <JobItem key={member.id} job={member} members={[]} selected={selected} />
              ))}
            </ul>
          </Box>
        )}
      </Flex>
    </li>
  );
}

function Jobs({ jobs, selected }: JobsProps) {
  const { t } = useI18n();
  const tops = jobs.filter((job) => job.group_id === null);
  if (jobs.length === 0) {
    return (
      <Text as="p" size="2" color="gray">
        {t('runPage.jobs.empty')}
      </Text>
    );
  }
  return (
    <Flex asChild direction="column" gap="1" m="0" p="0">
      <ul className="plain-list" aria-label={t('runPage.jobs.title')}>
        {tops.map((job) => (
          <JobItem
            key={job.id}
            job={job}
            members={jobs.filter((member) => member.group_id === job.id)}
            selected={selected}
          />
        ))}
      </ul>
    </Flex>
  );
}

function StepItem({ step, now }: StepItemProps) {
  const { t } = useI18n();
  const took = span(step.started_at, step.completed_at, now);
  return (
    <Flex align="center" justify="between" gap="3" wrap="wrap" py="1">
      <Text size="2">{step.name}</Text>
      <Flex align="center" gap="3">
        {took === null ? null : <Text size="1">{duration(t, took)}</Text>}
        <StateBadge status={step.status} conclusion={step.conclusion} source={step.conclusion_source} />
      </Flex>
    </Flex>
  );
}

function JobPanel({ run, job, allowed }: JobPanelProps) {
  const { t } = useI18n();
  const execution = useExecution(job.execution_id);
  const now = useNow(job.status !== 'completed');
  return (
    <Flex direction="column" gap="4">
      <Flex align="center" justify="between" gap="3" wrap="wrap">
        <Flex align="center" gap="3" wrap="wrap">
          <Heading as="h3" size="4">
            {job.name}
          </Heading>
          <StateBadge status={job.status} conclusion={job.conclusion} source={job.conclusion_source} />
        </Flex>
        {job.status !== 'completed' && allowed.has('run.cancel') ? (
          <Cancel key={job.id} run={run} job={job} />
        ) : null}
      </Flex>
      {job.execution_id === null ? (
        <Text as="p" size="2" color="gray">
          {t('runPage.panel.noExecution')}
        </Text>
      ) : null}
      {execution.isError ? <Failure error={execution.error} onRetry={() => execution.refetch()} /> : null}
      {execution.data !== undefined && execution.data.steps.length === 0 ? (
        <Text as="p" size="2" color="gray">
          {t('runPage.panel.noSteps')}
        </Text>
      ) : null}
      {execution.data !== undefined && execution.data.steps.length > 0 ? (
        <Flex asChild direction="column" gap="1" m="0" p="0">
          <ol className="plain-list" aria-label={t('runPage.panel.steps')}>
            {nest(execution.data.steps).map((node) => (
              <li key={node.step.id}>
                <StepItem step={node.step} now={now} />
                {node.pre.length + node.post.length === 0 ? null : (
                  <Box asChild pl="4" m="0">
                    <ol className="plain-list">
                      {[...node.pre, ...node.post].map((step) => (
                        <li key={step.id}>
                          <Flex align="center" gap="1">
                            <ChevronRight size={14} aria-hidden />
                            <Box flexGrow="1">
                              <StepItem step={step} now={now} />
                            </Box>
                          </Flex>
                        </li>
                      ))}
                    </ol>
                  </Box>
                )}
              </li>
            ))}
          </ol>
        </Flex>
      ) : null}
    </Flex>
  );
}

function RunView({ run }: RunViewProps) {
  const { t } = useI18n();
  const [params] = useSearchParams();
  const me = useMe();
  const project = useProject(run.project_id);
  const jobs = useJobs(run.id, run.attempt);
  const allowed = me.data && project.data ? permissionsOn(me.data, project.data) : new Set<Permission>();
  const selected = params.get('job');
  const job = jobs.data?.find((known) => known.id === selected);
  return (
    <Flex direction="column" gap="5">
      {project.data === undefined ? null : (
        <Text size="2">
          <Link asChild>
            <RouterLink to={`/projects/${project.data.path}`}>{project.data.path}</RouterLink>
          </Link>
        </Text>
      )}
      <Header run={run} jobs={jobs.data ?? []} allowed={allowed} />
      {run.notices.length === 0 ? null : (
        <Section title={t('runPage.notices.title')}>
          <Notices notices={run.notices} jobs={jobs.data ?? []} />
        </Section>
      )}
      <Grid columns={{ initial: '1', md: '5' }} gap="5" align="start">
        <Box gridColumn={{ initial: '1', md: 'span 2' }}>
          <Section title={t('runPage.jobs.title')}>
            {jobs.isPending ? <Loading /> : null}
            {jobs.isError ? <Failure error={jobs.error} onRetry={() => jobs.refetch()} /> : null}
            {jobs.data === undefined ? null : <Jobs jobs={jobs.data} selected={selected} />}
          </Section>
        </Box>
        <Box gridColumn={{ initial: '1', md: 'span 3' }}>
          <Section title={t('runPage.panel.title')}>
            {job === undefined ? (
              <Text as="p" size="2" color="gray">
                {t('runPage.panel.choose')}
              </Text>
            ) : (
              <JobPanel run={run} job={job} allowed={allowed} />
            )}
          </Section>
        </Box>
      </Grid>
    </Flex>
  );
}

export function RunPage() {
  const { t } = useI18n();
  const { run: id = '' } = useParams();
  useLive({ run: [id] });
  const run = useRun(id);
  if (run.isPending) {
    return <Loading />;
  }
  if (run.isError) {
    if (run.error instanceof ApiError && run.error.code === 'not_found') {
      return (
        <Flex direction="column" gap="3">
          <Heading as="h1" size="7">
            {t('notFound.title')}
          </Heading>
          <Text size="3">{t('runPage.notFound', { id })}</Text>
        </Flex>
      );
    }
    return <Failure error={run.error} onRetry={() => run.refetch()} />;
  }
  return <RunView run={run.data} />;
}
