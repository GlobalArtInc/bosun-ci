import { Button, Flex, Heading, Link, Table, Text } from '@radix-ui/themes';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { Plus } from 'lucide-react';
import type { FormEvent } from 'react';
import { Link as RouterLink, useNavigate } from 'react-router';
import type { Agent, CreatePoolRequest, Pool, TrustLevel } from '../api/client';
import { useApi } from '../api/context';
import { POOLS } from '../api/queries';
import { useI18n } from '../i18n/i18n';
import { ago, stamp, useNow } from '../runs/time';
import { CheckInput, listOf, SelectInput, TextInput, textOf } from '../shell/inputs';
import { Refusal } from '../shell/refusal';
import { accessOf, poolPath, TRUST, TrustBadge } from './agents';

interface PoolListProps {
  pools: Pool[];
  agents: Agent[];
}

const TRUST_LEVELS: TrustLevel[] = ['trusted', 'untrusted'];

function requestOf(form: FormData): CreatePoolRequest {
  return {
    name: textOf(form, 'name'),
    trust: textOf(form, 'trust') === 'untrusted' ? 'untrusted' : 'trusted',
    access: {
      all_projects: form.get('all_projects') !== null,
      organisations: listOf(form, 'organisations'),
      projects: listOf(form, 'projects'),
    },
  };
}

export function PoolList({ pools, agents }: PoolListProps) {
  const { t, language } = useI18n();
  const now = useNow(false);
  return (
    <Table.Root variant="surface" size="2">
      <Table.Header>
        <Table.Row>
          <Table.ColumnHeaderCell>{t('poolPage.column.pool')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('poolPage.column.trust')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('poolPage.column.access')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('poolPage.column.agents')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('runs.column.created')}</Table.ColumnHeaderCell>
        </Table.Row>
      </Table.Header>
      <Table.Body>
        {pools.map((pool) => {
          const members = agents.filter((agent) => agent.pool_id === pool.id);
          return (
            <Table.Row key={pool.id} align="center">
              <Table.RowHeaderCell>
                <Link asChild size="2" weight="medium">
                  <RouterLink to={poolPath(pool)}>{pool.name}</RouterLink>
                </Link>
              </Table.RowHeaderCell>
              <Table.Cell>
                <TrustBadge trust={pool.trust} />
              </Table.Cell>
              <Table.Cell>
                <Text size="2">{accessOf(t, pool.access)}</Text>
              </Table.Cell>
              <Table.Cell>
                <Text size="2">
                  {t('poolPage.agentCount', {
                    online: members.filter((agent) => agent.online).length,
                    count: members.length,
                  })}
                </Text>
              </Table.Cell>
              <Table.Cell>
                <Text size="2" title={stamp(language, pool.created_at)}>
                  {ago(language, pool.created_at, now)}
                </Text>
              </Table.Cell>
            </Table.Row>
          );
        })}
      </Table.Body>
    </Table.Root>
  );
}

export function CreatePool() {
  const { t } = useI18n();
  const api = useApi();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const create = useMutation({
    mutationFn: (request: CreatePoolRequest) => api.createPool(request, crypto.randomUUID()),
    onSuccess: (pool) => {
      queryClient.setQueryData<Pool[]>(POOLS, (pools) => (pools === undefined ? pools : [...pools, pool]));
      void navigate(poolPath(pool));
    },
  });

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    create.mutate(requestOf(new FormData(event.currentTarget)));
  };

  return (
    <form onSubmit={submit} aria-labelledby="create-pool" className="form">
      <Flex direction="column" gap="3">
        <Heading as="h3" size="3" id="create-pool">
          {t('poolPage.create.title')}
        </Heading>
        <Flex gap="3" wrap="wrap" align="start">
          <TextInput name="name" label={t('poolPage.create.name')} required maxLength={64} />
          <SelectInput
            name="trust"
            label={t('poolPage.column.trust')}
            defaultValue="trusted"
            options={TRUST_LEVELS.map((trust) => ({ value: trust, label: t(TRUST[trust].key) }))}
          />
        </Flex>
        <CheckInput name="all_projects" label={t('poolPage.create.allProjects')} />
        <Flex gap="3" wrap="wrap" align="start">
          <TextInput
            name="organisations"
            label={t('poolPage.create.organisations')}
            hint={t('poolPage.create.organisationsHint')}
            maxLength={4096}
          />
          <TextInput
            name="projects"
            label={t('poolPage.create.projects')}
            hint={t('poolPage.create.projectsHint')}
            maxLength={4096}
          />
        </Flex>
        <Flex>
          <Button type="submit" loading={create.isPending}>
            <Plus size={16} aria-hidden />
            {t('poolPage.create.submit')}
          </Button>
        </Flex>
        {create.isError ? <Refusal error={create.error} /> : null}
      </Flex>
    </form>
  );
}
