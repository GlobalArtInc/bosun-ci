import { AlertDialog, Badge, Button, Flex, Heading, Link, Spinner, Table, Text } from '@radix-ui/themes';
import { type QueryClient, useMutation, useQueryClient } from '@tanstack/react-query';
import { Ban, Play, Trash2 } from 'lucide-react';
import type { ComponentProps } from 'react';
import { Link as RouterLink } from 'react-router';
import type { Agent, EventFilter, Pool, PoolAccess, TrustLevel } from '../api/client';
import { useApi } from '../api/context';
import { AGENTS, useExecution, useJob } from '../api/queries';
import { type I18n, type Key, useI18n } from '../i18n/i18n';
import { ago, stamp, useNow } from '../runs/time';
import { Refusal } from '../shell/refusal';

type AgentState = 'idle' | 'busy' | 'offline';

type Color = ComponentProps<typeof Badge>['color'];

interface Look {
  key: Key;
  color: Color;
}

interface AgentProps {
  agent: Agent;
}

interface AgentTableProps {
  agents: Agent[];
}

interface AgentsByPoolProps {
  agents: Agent[];
  pools: Pool[];
}

interface CurrentJobProps {
  execution: string;
}

interface TrustBadgeProps {
  trust: TrustLevel;
}

interface LabelsProps {
  labels: string[];
  missing?: string[];
}

export const AGENT_EVENTS: EventFilter = {
  kind: ['queue.job', 'agent.status', 'execution.created', 'execution.status'],
};

const STATES: Record<AgentState, Look> = {
  idle: { key: 'agentsPage.state.idle', color: 'green' },
  busy: { key: 'agentsPage.state.busy', color: 'blue' },
  offline: { key: 'agentsPage.state.offline', color: 'gray' },
};

export const TRUST: Record<TrustLevel, Look> = {
  trusted: { key: 'agentsPage.trust.trusted', color: 'green' },
  untrusted: { key: 'agentsPage.trust.untrusted', color: 'orange' },
};

function agentState(agent: Agent): AgentState {
  if (!agent.online) {
    return 'offline';
  }
  return agent.execution_id === null ? 'idle' : 'busy';
}

export function poolPath(pool: Pool): string {
  return `/pools/${encodeURIComponent(pool.name)}`;
}

export function accessOf(t: I18n['t'], access: PoolAccess): string {
  if (access.all_projects) {
    return t('poolPage.access.all');
  }
  const named = [
    ...access.organisations.map((organisation) => organisation.slug),
    ...access.projects.map((project) => project.path),
  ];
  return named.length === 0 ? t('poolPage.access.none') : named.join(', ');
}

function replaced(queryClient: QueryClient, agent: Agent) {
  queryClient.setQueryData<Agent[]>(AGENTS, (agents) =>
    agents?.map((known) => (known.id === agent.id ? agent : known)),
  );
}

function removed(queryClient: QueryClient, agent: Agent) {
  queryClient.setQueryData<Agent[]>(AGENTS, (agents) => agents?.filter((known) => known.id !== agent.id));
}

export function TrustBadge({ trust }: TrustBadgeProps) {
  const { t } = useI18n();
  const look = TRUST[trust];
  return (
    <Badge color={look.color} variant="soft" size="2" highContrast>
      {t(look.key)}
    </Badge>
  );
}

export function Labels({ labels, missing = [] }: LabelsProps) {
  if (labels.length === 0) {
    return <Text size="2">—</Text>;
  }
  return (
    <Flex gap="1" wrap="wrap">
      {labels.map((label) => (
        <Badge
          key={label}
          color={missing.includes(label) ? 'amber' : 'gray'}
          variant={missing.includes(label) ? 'solid' : 'soft'}
          highContrast
        >
          {label}
        </Badge>
      ))}
    </Flex>
  );
}

function AgentStateCell({ agent }: AgentProps) {
  const { t, language } = useI18n();
  const now = useNow(false);
  const look = STATES[agentState(agent)];
  return (
    <Flex direction="column" gap="1" align="start">
      <Flex gap="2" wrap="wrap">
        <Badge color={look.color} variant="soft" size="2" highContrast>
          {t(look.key)}
        </Badge>
        {agent.drain ? (
          <Badge color="orange" variant="outline" size="2" highContrast>
            {t('agentsPage.state.draining')}
          </Badge>
        ) : null}
      </Flex>
      {agent.online || agent.last_seen_at === null ? null : (
        <Text size="1" color="gray" title={stamp(language, agent.last_seen_at)}>
          {t('agentsPage.seen', { time: ago(language, agent.last_seen_at, now) })}
        </Text>
      )}
    </Flex>
  );
}

function CurrentJob({ execution }: CurrentJobProps) {
  const read = useExecution(execution);
  const job = useJob(read.data?.job_id ?? null);
  if (job.data === undefined) {
    return read.isError || job.isError ? <Text size="2">—</Text> : <Spinner size="1" />;
  }
  return (
    <Link asChild size="2">
      <RouterLink to={`/runs/${job.data.run_id}?job=${job.data.id}`}>{job.data.name}</RouterLink>
    </Link>
  );
}

function AgentActions({ agent }: AgentProps) {
  const { t } = useI18n();
  const api = useApi();
  const queryClient = useQueryClient();
  const drain = useMutation({
    mutationFn: (drained: boolean) => api.drain(agent.id, drained),
    onSuccess: (changed) => replaced(queryClient, changed),
  });
  const remove = useMutation({
    mutationFn: () => api.deleteAgent(agent.id),
    onSuccess: () => removed(queryClient, agent),
  });
  return (
    <Flex direction="column" gap="2" align="start">
      <Flex gap="2" wrap="wrap">
        <Button
          size="1"
          variant="soft"
          color={agent.drain ? 'green' : 'orange'}
          loading={drain.isPending}
          onClick={() => drain.mutate(!agent.drain)}
        >
          {agent.drain ? <Play size={14} aria-hidden /> : <Ban size={14} aria-hidden />}
          {agent.drain ? t('agentsPage.lift') : t('agentsPage.drain')}
        </Button>
        <AlertDialog.Root>
          <AlertDialog.Trigger>
            <Button size="1" variant="soft" color="red" loading={remove.isPending}>
              <Trash2 size={14} aria-hidden />
              {t('agentsPage.delete')}
            </Button>
          </AlertDialog.Trigger>
          <AlertDialog.Content maxWidth="440px">
            <AlertDialog.Title>{t('agentsPage.deleteQuestion', { name: agent.name })}</AlertDialog.Title>
            <AlertDialog.Description size="2">{t('agentsPage.deleteEffect')}</AlertDialog.Description>
            <Flex gap="3" mt="4" justify="end">
              <AlertDialog.Cancel>
                <Button variant="soft" color="gray">
                  {t('agentsPage.keep')}
                </Button>
              </AlertDialog.Cancel>
              <AlertDialog.Action>
                <Button color="red" onClick={() => remove.mutate()}>
                  {t('agentsPage.delete')}
                </Button>
              </AlertDialog.Action>
            </Flex>
          </AlertDialog.Content>
        </AlertDialog.Root>
      </Flex>
      {drain.isError ? <Refusal error={drain.error} /> : null}
      {remove.isError ? <Refusal error={remove.error} /> : null}
    </Flex>
  );
}

export function AgentTable({ agents }: AgentTableProps) {
  const { t } = useI18n();
  if (agents.length === 0) {
    return (
      <Text as="p" size="2" color="gray">
        {t('agentsPage.empty')}
      </Text>
    );
  }
  return (
    <Table.Root variant="surface" size="2">
      <Table.Header>
        <Table.Row>
          <Table.ColumnHeaderCell>{t('agentsPage.column.agent')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('agentsPage.column.state')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('agentsPage.column.job')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('agentsPage.column.version')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('agentsPage.column.labels')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('agentsPage.column.capabilities')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('form.actions')}</Table.ColumnHeaderCell>
        </Table.Row>
      </Table.Header>
      <Table.Body>
        {agents.map((agent) => (
          <Table.Row key={agent.id} align="center">
            <Table.RowHeaderCell>
              <Text size="2" weight="medium">
                {agent.name}
              </Text>
            </Table.RowHeaderCell>
            <Table.Cell>
              <AgentStateCell agent={agent} />
            </Table.Cell>
            <Table.Cell>
              {agent.execution_id === null ? (
                <Text size="2">—</Text>
              ) : (
                <CurrentJob execution={agent.execution_id} />
              )}
            </Table.Cell>
            <Table.Cell>
              <Text size="2">{agent.version ?? '—'}</Text>
            </Table.Cell>
            <Table.Cell>
              <Labels labels={agent.labels} />
            </Table.Cell>
            <Table.Cell>
              <Labels labels={agent.capabilities} />
            </Table.Cell>
            <Table.Cell>
              <AgentActions agent={agent} />
            </Table.Cell>
          </Table.Row>
        ))}
      </Table.Body>
    </Table.Root>
  );
}

export function AgentsByPool({ agents, pools }: AgentsByPoolProps) {
  return (
    <Flex direction="column" gap="5">
      {pools.map((pool) => (
        <section key={pool.id} aria-label={pool.name}>
          <Flex direction="column" gap="2">
            <Flex align="center" gap="3" wrap="wrap">
              <Heading as="h3" size="3">
                <Link asChild>
                  <RouterLink to={poolPath(pool)}>{pool.name}</RouterLink>
                </Link>
              </Heading>
              <TrustBadge trust={pool.trust} />
            </Flex>
            <AgentTable agents={agents.filter((agent) => agent.pool_id === pool.id)} />
          </Flex>
        </section>
      ))}
    </Flex>
  );
}
