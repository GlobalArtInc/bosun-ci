import { Badge, Button, Code, Flex, Heading, Link, Select, Table, Text, TextField } from '@radix-ui/themes';
import { FolderGit2 } from 'lucide-react';
import type { FormEvent } from 'react';
import { Link as RouterLink, useParams, useSearchParams } from 'react-router';
import { ApiError, type EventFilter, type Project, type RunFilters, type Workflow } from '../api/client';
import { useProjectAt, useWorkflows } from '../api/queries';
import { type Key, useI18n } from '../i18n/i18n';
import { useLive } from '../live/live';
import { Runs } from '../runs/run-list';
import { STATUS_FILTERS, StateBadge } from '../runs/state';
import { ago, stamp, useNow } from '../runs/time';
import { Section } from '../shell/section';
import { Failure, Loading } from '../shell/status';

interface ProjectProps {
  project: Project;
}

interface WorkflowsProps {
  workflows: Workflow[];
}

interface FiltersProps {
  workflows: Workflow[];
}

const ANY = 'any';

const LIMIT = 25;

const TEXT_FILTERS: [keyof RunFilters, Key][] = [
  ['branch', 'projectPage.filter.branch'],
  ['event', 'projectPage.filter.event'],
  ['actor', 'projectPage.filter.actor'],
];

const FILTERS = ['workflow', 'branch', 'event', 'status', 'actor'] as const;

function filtersOf(params: URLSearchParams): RunFilters {
  const filters: RunFilters = {};
  for (const name of FILTERS) {
    const value = params.get(name)?.trim();
    if (value) {
      filters[name] = value;
    }
  }
  return filters;
}

function WorkflowTable({ workflows }: WorkflowsProps) {
  const { t, language } = useI18n();
  const now = useNow(workflows.some((workflow) => workflow.latest_run?.status !== 'completed'));
  if (workflows.length === 0) {
    return (
      <Text as="p" size="2" color="gray">
        {t('projectPage.workflows.empty')}
      </Text>
    );
  }
  return (
    <Table.Root variant="surface" size="2">
      <Table.Header>
        <Table.Row>
          <Table.ColumnHeaderCell>{t('projectPage.workflows.name')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('projectPage.workflows.latest')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('runs.column.created')}</Table.ColumnHeaderCell>
        </Table.Row>
      </Table.Header>
      <Table.Body>
        {workflows.map((workflow) => {
          const run = workflow.latest_run;
          return (
            <Table.Row key={workflow.id} align="center">
              <Table.RowHeaderCell>
                <Flex align="center" gap="2" wrap="wrap">
                  <Link asChild size="2" weight="medium">
                    <RouterLink to={`?workflow=${workflow.id}`}>{workflow.name}</RouterLink>
                  </Link>
                  {workflow.enabled ? null : (
                    <Badge color="gray" variant="outline" highContrast>
                      {t('projectPage.workflows.disabled')}
                    </Badge>
                  )}
                </Flex>
              </Table.RowHeaderCell>
              <Table.Cell>
                {run === null ? (
                  <Text size="2" color="gray">
                    {t('projectPage.workflows.never')}
                  </Text>
                ) : (
                  <Flex align="center" gap="2" wrap="wrap">
                    <StateBadge status={run.status} conclusion={run.conclusion} />
                    <Link asChild size="2">
                      <RouterLink to={`/runs/${run.id}`}>#{run.number}</RouterLink>
                    </Link>
                  </Flex>
                )}
              </Table.Cell>
              <Table.Cell>
                {run === null ? null : (
                  <Text size="2" title={stamp(language, run.created_at)}>
                    {ago(language, run.created_at, now)}
                  </Text>
                )}
              </Table.Cell>
            </Table.Row>
          );
        })}
      </Table.Body>
    </Table.Root>
  );
}

function Filters({ workflows }: FiltersProps) {
  const { t } = useI18n();
  const [params, setParams] = useSearchParams();
  const filters = filtersOf(params);

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const next = new URLSearchParams();
    for (const name of FILTERS) {
      const value = String(form.get(name) ?? '').trim();
      if (value !== '' && value !== ANY) {
        next.set(name, value);
      }
    }
    setParams(next);
  };

  return (
    <form onSubmit={submit} aria-label={t('projectPage.filter.label')} key={params.toString()}>
      <Flex gap="3" wrap="wrap" align="end">
        <Flex direction="column" gap="1">
          <Text as="label" size="2" weight="medium" htmlFor="filter-workflow">
            {t('projectPage.filter.workflow')}
          </Text>
          <Select.Root name="workflow" defaultValue={filters.workflow ?? ANY}>
            <Select.Trigger id="filter-workflow" />
            <Select.Content position="popper">
              <Select.Item value={ANY}>{t('projectPage.filter.any')}</Select.Item>
              {workflows.map((workflow) => (
                <Select.Item key={workflow.id} value={workflow.id}>
                  {workflow.name}
                </Select.Item>
              ))}
            </Select.Content>
          </Select.Root>
        </Flex>
        <Flex direction="column" gap="1">
          <Text as="label" size="2" weight="medium" htmlFor="filter-status">
            {t('projectPage.filter.status')}
          </Text>
          <Select.Root name="status" defaultValue={filters.status ?? ANY}>
            <Select.Trigger id="filter-status" />
            <Select.Content position="popper">
              <Select.Item value={ANY}>{t('projectPage.filter.any')}</Select.Item>
              {Object.entries(STATUS_FILTERS).map(([value, key]) => (
                <Select.Item key={value} value={value}>
                  {t(key)}
                </Select.Item>
              ))}
            </Select.Content>
          </Select.Root>
        </Flex>
        {TEXT_FILTERS.map(([name, key]) => (
          <Flex key={name} direction="column" gap="1">
            <Text as="label" size="2" weight="medium" htmlFor={`filter-${name}`}>
              {t(key)}
            </Text>
            <TextField.Root
              id={`filter-${name}`}
              name={name}
              defaultValue={filters[name] ?? ''}
              maxLength={255}
            />
          </Flex>
        ))}
        <Flex gap="2">
          <Button type="submit">{t('projectPage.filter.apply')}</Button>
          <Button type="button" variant="soft" color="gray" onClick={() => setParams(new URLSearchParams())}>
            {t('projectPage.filter.clear')}
          </Button>
        </Flex>
      </Flex>
    </form>
  );
}

function ProjectView({ project }: ProjectProps) {
  const { t } = useI18n();
  const [params] = useSearchParams();
  const workflows = useWorkflows(project.id);
  const filters: RunFilters = { ...filtersOf(params), project: project.id };
  const live: EventFilter = { project: [project.id], kind: ['run.created', 'run.status'] };
  useLive(live);
  return (
    <Flex direction="column" gap="5">
      <Flex direction="column" gap="1">
        <Heading as="h1" size="7">
          {project.name}
        </Heading>
        <Flex align="center" gap="3" wrap="wrap">
          <Code variant="ghost" size="2" color="gray">
            {project.path}
          </Code>
          {project.repository === null ? null : (
            <Flex as="span" align="center" gap="1">
              <FolderGit2 size={14} aria-hidden />
              <Text size="2">{`${project.repository.owner}/${project.repository.name}`}</Text>
            </Flex>
          )}
        </Flex>
      </Flex>
      <Section title={t('projectPage.workflows.title')}>
        {workflows.isPending ? <Loading /> : null}
        {workflows.isError ? <Failure error={workflows.error} onRetry={() => workflows.refetch()} /> : null}
        {workflows.data === undefined ? null : <WorkflowTable workflows={workflows.data} />}
      </Section>
      <Section title={t('projectPage.runs.title')}>
        <Filters workflows={workflows.data ?? []} />
        <Runs filters={filters} limit={LIMIT} />
      </Section>
    </Flex>
  );
}

export function ProjectPage() {
  const { t } = useI18n();
  const { organisation = '', project: slug = '' } = useParams();
  const project = useProjectAt(organisation, slug);
  if (project.isPending) {
    return <Loading />;
  }
  if (project.isError) {
    if (project.error instanceof ApiError && project.error.code === 'not_found') {
      return (
        <Flex direction="column" gap="3">
          <Heading as="h1" size="7">
            {t('notFound.title')}
          </Heading>
          <Text size="3">{t('projectPage.notFound', { path: `${organisation}/${slug}` })}</Text>
        </Flex>
      );
    }
    return <Failure error={project.error} onRetry={() => project.refetch()} />;
  }
  return <ProjectView project={project.data} />;
}
