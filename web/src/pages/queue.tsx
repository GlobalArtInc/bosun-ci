import { Flex, Heading, Text } from '@radix-ui/themes';
import { useMemo } from 'react';
import { AGENT_EVENTS, AgentsByPool } from '../agents/agents';
import { AliasList, SetAlias } from '../agents/aliases';
import { CreatePool, PoolList } from '../agents/pools';
import { QueueTable } from '../agents/queue';
import type { EventFilter } from '../api/client';
import { useAgents, useAliases, usePools, useProjects, useQueue } from '../api/queries';
import { useI18n } from '../i18n/i18n';
import { useLive } from '../live/live';
import { managesAgents } from '../session/permissions';
import { useMe } from '../session/session';
import { Section } from '../shell/section';
import { Failure, Loading } from '../shell/status';

const QUEUE_EVENTS: EventFilter = { kind: ['queue.job'] };

function Queue() {
  const queue = useQueue();
  const projects = useProjects();
  const byId = useMemo(
    () => new Map((projects.data ?? []).map((project) => [project.id, project])),
    [projects.data],
  );
  if (queue.isPending) {
    return <Loading />;
  }
  if (queue.isError) {
    return <Failure error={queue.error} onRetry={() => queue.refetch()} />;
  }
  return <QueueTable entries={queue.data} projects={byId} />;
}

function Managed() {
  const { t } = useI18n();
  const agents = useAgents();
  const pools = usePools();
  const aliases = useAliases();
  const broken = [agents, pools, aliases].find((query) => query.isError);
  return (
    <>
      <Section title={t('queuePage.agents.title')}>
        {broken?.error ? <Failure error={broken.error} onRetry={() => broken.refetch()} /> : null}
        {agents.data !== undefined && pools.data !== undefined ? (
          <AgentsByPool agents={agents.data} pools={pools.data} />
        ) : null}
        {(agents.isPending || pools.isPending) && broken === undefined ? <Loading /> : null}
      </Section>
      <Section title={t('queuePage.pools.title')}>
        {pools.data === undefined ? null : <PoolList pools={pools.data} agents={agents.data ?? []} />}
        <CreatePool />
      </Section>
      <Section title={t('queuePage.aliases.title')}>
        <Text as="p" size="2">
          {t('queuePage.aliases.explanation')}
        </Text>
        {aliases.data === undefined ? null : <AliasList aliases={aliases.data} pools={pools.data ?? []} />}
        <SetAlias pools={pools.data ?? []} />
      </Section>
    </>
  );
}

export function QueuePage() {
  const { t } = useI18n();
  const me = useMe();
  const allowed = me.data ? managesAgents(me.data) : false;
  useLive(allowed ? AGENT_EVENTS : QUEUE_EVENTS);
  return (
    <Flex direction="column" gap="5">
      <Heading as="h1" size="7">
        {allowed ? t('shell.nav.agents') : t('shell.nav.queue')}
      </Heading>
      <Section title={t('queuePage.queue.title')}>
        <Queue />
      </Section>
      {allowed ? <Managed /> : null}
    </Flex>
  );
}
