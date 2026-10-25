import { Button, Code, Flex, Heading, Spinner, Table, Text } from '@radix-ui/themes';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { Tag } from 'lucide-react';
import { type FormEvent, useState } from 'react';
import type { Capability, LabelAlias, Pool, SetLabelAliasRequest } from '../api/client';
import { useApi } from '../api/context';
import { ALIASES, useOrganisation } from '../api/queries';
import { useI18n } from '../i18n/i18n';
import { ago, stamp, useNow } from '../runs/time';
import { SelectInput, TextInput, textOf } from '../shell/inputs';
import { Refusal } from '../shell/refusal';

interface AliasListProps {
  aliases: LabelAlias[];
  pools: Pool[];
}

interface SetAliasProps {
  pools: Pool[];
}

interface OrganisationSlugProps {
  id: string;
}

const ANY = 'any';

const CAPABILITIES: Capability[] = ['shell', 'podman'];

function nullable(form: FormData, name: string): string | null {
  const value = textOf(form, name);
  return value === '' || value === ANY ? null : value;
}

function requestOf(form: FormData): SetLabelAliasRequest {
  const capability = nullable(form, 'capability');
  return {
    label: textOf(form, 'label'),
    organisation: nullable(form, 'organisation'),
    pool: nullable(form, 'pool'),
    capability: CAPABILITIES.find((known) => known === capability) ?? null,
    image: nullable(form, 'image'),
  };
}

function OrganisationSlug({ id }: OrganisationSlugProps) {
  const organisation = useOrganisation(id);
  if (organisation.data === undefined) {
    return organisation.isError ? <Code variant="ghost">{id}</Code> : <Spinner size="1" />;
  }
  return <Text size="2">{organisation.data.slug}</Text>;
}

export function AliasList({ aliases, pools }: AliasListProps) {
  const { t, language } = useI18n();
  const now = useNow(false);
  if (aliases.length === 0) {
    return (
      <Text as="p" size="2" color="gray">
        {t('queuePage.aliases.empty')}
      </Text>
    );
  }
  const names = new Map(pools.map((pool) => [pool.id, pool.name]));
  return (
    <Table.Root variant="surface" size="2">
      <Table.Header>
        <Table.Row>
          <Table.ColumnHeaderCell>{t('queuePage.aliases.label')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('queuePage.aliases.scope')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('poolPage.column.pool')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('queuePage.aliases.capability')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('queuePage.aliases.image')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('settingsPage.column.updated')}</Table.ColumnHeaderCell>
        </Table.Row>
      </Table.Header>
      <Table.Body>
        {aliases.map((alias) => (
          <Table.Row key={alias.id} align="center">
            <Table.RowHeaderCell>
              <Code variant="ghost" size="2">
                {alias.label}
              </Code>
            </Table.RowHeaderCell>
            <Table.Cell>
              {alias.organisation_id === null ? (
                <Text size="2">{t('queuePage.aliases.installation')}</Text>
              ) : (
                <OrganisationSlug id={alias.organisation_id} />
              )}
            </Table.Cell>
            <Table.Cell>
              <Text size="2">
                {alias.pool_id === null
                  ? t('queuePage.aliases.anyPool')
                  : (names.get(alias.pool_id) ?? alias.pool_id)}
              </Text>
            </Table.Cell>
            <Table.Cell>
              <Text size="2">{alias.capability ?? t('queuePage.aliases.either')}</Text>
            </Table.Cell>
            <Table.Cell>
              {alias.image === null ? (
                <Text size="2">—</Text>
              ) : (
                <Code variant="ghost" size="2">
                  {alias.image}
                </Code>
              )}
            </Table.Cell>
            <Table.Cell>
              <Text size="2" title={stamp(language, alias.updated_at)}>
                {ago(language, alias.updated_at, now)}
              </Text>
            </Table.Cell>
          </Table.Row>
        ))}
      </Table.Body>
    </Table.Root>
  );
}

export function SetAlias({ pools }: SetAliasProps) {
  const { t } = useI18n();
  const api = useApi();
  const queryClient = useQueryClient();
  const [generation, setGeneration] = useState(0);
  const set = useMutation({
    mutationFn: (request: SetLabelAliasRequest) => api.setAlias(request),
    onSuccess: (alias) => {
      queryClient.setQueryData<LabelAlias[]>(ALIASES, (aliases) => {
        if (aliases === undefined) {
          return aliases;
        }
        const others = aliases.filter((known) => known.id !== alias.id);
        return others.length === aliases.length
          ? [...aliases, alias]
          : aliases.map((known) => (known.id === alias.id ? alias : known));
      });
      setGeneration((count) => count + 1);
    },
  });

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    set.mutate(requestOf(new FormData(event.currentTarget)));
  };

  return (
    <form onSubmit={submit} aria-labelledby="set-alias" key={generation} className="form">
      <Flex direction="column" gap="3">
        <Heading as="h3" size="3" id="set-alias">
          {t('queuePage.aliases.set')}
        </Heading>
        <Flex gap="3" wrap="wrap" align="start">
          <TextInput name="label" label={t('queuePage.aliases.label')} required maxLength={64} />
          <TextInput
            name="organisation"
            label={t('queuePage.aliases.organisation')}
            hint={t('queuePage.aliases.organisationHint')}
            maxLength={64}
          />
        </Flex>
        <Flex gap="3" wrap="wrap" align="start">
          <SelectInput
            name="pool"
            label={t('poolPage.column.pool')}
            defaultValue={ANY}
            options={[
              { value: ANY, label: t('queuePage.aliases.anyPool') },
              ...pools.map((pool) => ({ value: pool.name, label: pool.name })),
            ]}
          />
          <SelectInput
            name="capability"
            label={t('queuePage.aliases.capability')}
            defaultValue={ANY}
            options={[
              { value: ANY, label: t('queuePage.aliases.either') },
              ...CAPABILITIES.map((capability) => ({ value: capability, label: capability })),
            ]}
          />
          <TextInput
            name="image"
            label={t('queuePage.aliases.image')}
            hint={t('queuePage.aliases.imageHint')}
          />
        </Flex>
        <Flex>
          <Button type="submit" loading={set.isPending}>
            <Tag size={16} aria-hidden />
            {t('queuePage.aliases.submit')}
          </Button>
        </Flex>
        {set.isSuccess ? (
          <Text size="2" role="status">
            {t('queuePage.aliases.saved', { label: set.data.label })}
          </Text>
        ) : null}
        {set.isError ? <Refusal error={set.error} /> : null}
      </Flex>
    </form>
  );
}
