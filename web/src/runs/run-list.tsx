import { Button, Code, Flex, Link, Table, Text } from '@radix-ui/themes';
import { GitBranch } from 'lucide-react';
import { Link as RouterLink } from 'react-router';
import type { Project, Run, RunFilters } from '../api/client';
import { useRuns } from '../api/queries';
import { useI18n } from '../i18n/i18n';
import { Failure, Loading } from '../shell/status';
import { actorName, refName } from './labels';
import { StateBadge } from './state';
import { ago, duration, span, stamp, useNow } from './time';

interface RunTriggerProps {
  run: Run;
}

interface RunTableProps {
  runs: Run[];
  projects?: Map<string, Project>;
}

interface RunsProps {
  filters: RunFilters;
  limit: number;
  projects?: Map<string, Project>;
}

export function RunTrigger({ run }: RunTriggerProps) {
  const ref = refName(run.ref);
  return (
    <Flex as="span" align="center" gap="2" wrap="wrap">
      <Code variant="ghost" size="2">
        {run.event}
      </Code>
      {ref === null ? null : (
        <Flex as="span" align="center" gap="1">
          <GitBranch size={14} aria-hidden />
          <Text size="2">{ref}</Text>
        </Flex>
      )}
    </Flex>
  );
}

function RunTable({ runs, projects }: RunTableProps) {
  const { t, language } = useI18n();
  const now = useNow(runs.some((run) => run.status !== 'completed'));
  return (
    <Table.Root variant="surface" size="2">
      <Table.Header>
        <Table.Row>
          <Table.ColumnHeaderCell>{t('runs.column.status')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('runs.column.run')}</Table.ColumnHeaderCell>
          {projects === undefined ? null : (
            <Table.ColumnHeaderCell>{t('runs.column.project')}</Table.ColumnHeaderCell>
          )}
          <Table.ColumnHeaderCell>{t('runs.column.trigger')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('runs.column.actor')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('runs.column.created')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('runs.column.duration')}</Table.ColumnHeaderCell>
        </Table.Row>
      </Table.Header>
      <Table.Body>
        {runs.map((run) => {
          const took = span(run.started_at, run.completed_at, now);
          const project = projects?.get(run.project_id);
          return (
            <Table.Row key={run.id} align="center">
              <Table.Cell>
                <StateBadge status={run.status} conclusion={run.conclusion} />
              </Table.Cell>
              <Table.RowHeaderCell>
                <Link asChild size="2" weight="medium">
                  <RouterLink to={`/runs/${run.id}`}>
                    {run.display_name} #{run.number}
                  </RouterLink>
                </Link>
              </Table.RowHeaderCell>
              {projects === undefined ? null : (
                <Table.Cell>
                  {project === undefined ? null : (
                    <Link asChild size="2">
                      <RouterLink to={`/projects/${project.path}`}>{project.path}</RouterLink>
                    </Link>
                  )}
                </Table.Cell>
              )}
              <Table.Cell>
                <RunTrigger run={run} />
              </Table.Cell>
              <Table.Cell>
                <Text size="2">{actorName(run.actor)}</Text>
              </Table.Cell>
              <Table.Cell>
                <Text size="2" title={stamp(language, run.created_at)}>
                  {ago(language, run.created_at, now)}
                </Text>
              </Table.Cell>
              <Table.Cell>
                <Text size="2">{took === null ? '—' : duration(t, took)}</Text>
              </Table.Cell>
            </Table.Row>
          );
        })}
      </Table.Body>
    </Table.Root>
  );
}

export function Runs({ filters, limit, projects }: RunsProps) {
  const { t } = useI18n();
  const runs = useRuns(filters, limit);
  if (runs.isPending) {
    return <Loading />;
  }
  if (runs.isError) {
    return <Failure error={runs.error} onRetry={() => runs.refetch()} />;
  }
  const items = runs.data.pages.flatMap((page) => page.items);
  if (items.length === 0) {
    return (
      <Text as="p" size="2" color="gray">
        {t('runs.empty')}
      </Text>
    );
  }
  return (
    <Flex direction="column" gap="3">
      <RunTable runs={items} projects={projects} />
      {runs.hasNextPage ? (
        <Flex>
          <Button variant="soft" loading={runs.isFetchingNextPage} onClick={() => runs.fetchNextPage()}>
            {t('runs.more')}
          </Button>
        </Flex>
      ) : null}
    </Flex>
  );
}
