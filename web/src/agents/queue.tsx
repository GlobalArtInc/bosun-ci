import { Link, Table, Text } from '@radix-ui/themes';
import { Link as RouterLink } from 'react-router';
import type { Project, QueuedJob } from '../api/client';
import { type I18n, useI18n } from '../i18n/i18n';
import { StateBadge, WAITING } from '../runs/state';
import { duration, useNow } from '../runs/time';
import { Labels } from './agents';

interface QueueTableProps {
  entries: QueuedJob[];
  projects: Map<string, Project>;
}

function reasonOf(t: I18n['t'], entry: QueuedJob): string {
  const { status, waiting_reason: reason } = entry.job;
  if (status === 'waiting') {
    return reason === null ? t('state.status.waiting') : t(WAITING[reason]);
  }
  if (entry.missing_labels.length > 0) {
    return t('queuePage.reason.missing', { labels: entry.missing_labels.join(', ') });
  }
  return t('queuePage.reason.agent');
}

export function QueueTable({ entries, projects }: QueueTableProps) {
  const { t } = useI18n();
  const now = useNow(entries.length > 0);
  if (entries.length === 0) {
    return (
      <Text as="p" size="2" color="gray">
        {t('home.queue.empty')}
      </Text>
    );
  }
  return (
    <Table.Root variant="surface" size="2">
      <Table.Header>
        <Table.Row>
          <Table.ColumnHeaderCell>{t('queuePage.column.job')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('runs.column.project')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('runs.column.status')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('queuePage.column.reason')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('queuePage.column.waiting')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('agentsPage.column.labels')}</Table.ColumnHeaderCell>
        </Table.Row>
      </Table.Header>
      <Table.Body>
        {entries.map((entry) => {
          const { job } = entry;
          const project = projects.get(entry.project_id);
          return (
            <Table.Row key={job.id} align="center">
              <Table.RowHeaderCell>
                <Link asChild size="2" weight="medium">
                  <RouterLink to={`/runs/${job.run_id}?job=${job.id}`}>{job.name}</RouterLink>
                </Link>
              </Table.RowHeaderCell>
              <Table.Cell>
                {project === undefined ? null : (
                  <Link asChild size="2">
                    <RouterLink to={`/projects/${project.path}`}>{project.path}</RouterLink>
                  </Link>
                )}
              </Table.Cell>
              <Table.Cell>
                <StateBadge status={job.status} conclusion={job.conclusion} />
              </Table.Cell>
              <Table.Cell>
                <Text size="2" color={entry.missing_labels.length > 0 ? 'amber' : undefined}>
                  {reasonOf(t, entry)}
                </Text>
              </Table.Cell>
              <Table.Cell>
                <Text size="2">
                  {entry.since === null ? '—' : duration(t, Math.max(0, now - Date.parse(entry.since)))}
                </Text>
              </Table.Cell>
              <Table.Cell>
                <Labels labels={job.labels} missing={entry.missing_labels} />
              </Table.Cell>
            </Table.Row>
          );
        })}
      </Table.Body>
    </Table.Root>
  );
}
