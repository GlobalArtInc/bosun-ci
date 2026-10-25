import { Callout, DataList, Flex, Heading, Text } from '@radix-ui/themes';
import { ShieldAlert } from 'lucide-react';
import { useParams } from 'react-router';
import { AGENT_EVENTS, AgentTable, accessOf, TrustBadge } from '../agents/agents';
import { Registration } from '../agents/registration';
import type { Pool } from '../api/client';
import { useAgents, usePools } from '../api/queries';
import { useI18n } from '../i18n/i18n';
import { useLive } from '../live/live';
import { stamp } from '../runs/time';
import { managesAgents } from '../session/permissions';
import { useMe } from '../session/session';
import { Section } from '../shell/section';
import { Failure, Loading } from '../shell/status';

interface PoolViewProps {
  pool: Pool;
}

interface PoolNamedProps {
  name: string;
}

function PoolView({ pool }: PoolViewProps) {
  const { t, language } = useI18n();
  const agents = useAgents();
  return (
    <Flex direction="column" gap="5">
      <Flex align="center" gap="3" wrap="wrap">
        <Heading as="h1" size="7">
          {pool.name}
        </Heading>
        <TrustBadge trust={pool.trust} />
      </Flex>
      {pool.trust === 'untrusted' ? (
        <Callout.Root color="orange" role="note">
          <Callout.Icon>
            <ShieldAlert size={16} />
          </Callout.Icon>
          <Callout.Text>{t('poolPage.untrusted')}</Callout.Text>
        </Callout.Root>
      ) : null}
      <DataList.Root orientation={{ initial: 'vertical', sm: 'horizontal' }} size="2">
        <DataList.Item>
          <DataList.Label>{t('poolPage.column.access')}</DataList.Label>
          <DataList.Value>{accessOf(t, pool.access)}</DataList.Value>
        </DataList.Item>
        <DataList.Item>
          <DataList.Label>{t('runs.column.created')}</DataList.Label>
          <DataList.Value>{stamp(language, pool.created_at)}</DataList.Value>
        </DataList.Item>
      </DataList.Root>
      <Section title={t('queuePage.agents.title')}>
        {agents.isPending ? <Loading /> : null}
        {agents.isError ? <Failure error={agents.error} onRetry={() => agents.refetch()} /> : null}
        {agents.data === undefined ? null : (
          <AgentTable agents={agents.data.filter((agent) => agent.pool_id === pool.id)} />
        )}
      </Section>
      <Section title={t('poolPage.token.title')}>
        <Registration pool={pool} />
      </Section>
    </Flex>
  );
}

function PoolNamed({ name }: PoolNamedProps) {
  const { t } = useI18n();
  const pools = usePools();
  useLive(AGENT_EVENTS);
  if (pools.isPending) {
    return <Loading />;
  }
  if (pools.isError) {
    return <Failure error={pools.error} onRetry={() => pools.refetch()} />;
  }
  const pool = pools.data.find((known) => known.name === name);
  if (pool === undefined) {
    return (
      <Flex direction="column" gap="3">
        <Heading as="h1" size="7">
          {t('notFound.title')}
        </Heading>
        <Text size="3">{t('poolPage.notFound', { name })}</Text>
      </Flex>
    );
  }
  return <PoolView pool={pool} />;
}

export function PoolPage() {
  const { t } = useI18n();
  const { pool = '' } = useParams();
  const me = useMe();
  if (me.data && !managesAgents(me.data)) {
    return (
      <Text as="p" size="3">
        {t('poolPage.notAllowed')}
      </Text>
    );
  }
  return <PoolNamed name={pool} />;
}
