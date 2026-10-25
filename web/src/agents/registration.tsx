import { Button, Callout, Code, Flex, Text } from '@radix-ui/themes';
import { useMutation } from '@tanstack/react-query';
import { Copy, KeyRound, TerminalSquare } from 'lucide-react';
import { type FormEvent, useState } from 'react';
import type { Pool, RegistrationTokenCreated, RegistrationTokenRequest } from '../api/client';
import { useApi } from '../api/context';
import { type Key, useI18n } from '../i18n/i18n';
import { stamp } from '../runs/time';
import { CheckInput, SelectInput, textOf } from '../shell/inputs';
import { Refusal } from '../shell/refusal';

interface RegistrationProps {
  pool: Pool;
}

interface InstallCommandProps {
  created: RegistrationTokenCreated;
  onDone: () => void;
}

interface Lifetime {
  name: string;
  key: Key;
  hours: number;
}

const DAY: Lifetime = { name: 'day', key: 'poolPage.token.day', hours: 24 };

const LIFETIMES: Lifetime[] = [
  { name: 'hour', key: 'poolPage.token.hour', hours: 1 },
  DAY,
  { name: 'week', key: 'poolPage.token.week', hours: 24 * 7 },
  { name: 'month', key: 'poolPage.token.month', hours: 24 * 30 },
];

const HOUR_MS = 3_600_000;

export function installCommand(origin: string, token: string): string {
  return `curl -fsSL ${origin}/install-agent.sh | sudo sh -s -- --token ${token}`;
}

function requestOf(pool: Pool, form: FormData): RegistrationTokenRequest {
  const chosen = textOf(form, 'expires');
  const lifetime = LIFETIMES.find((known) => known.name === chosen) ?? DAY;
  return {
    pool: pool.name,
    reusable: form.get('reusable') !== null,
    expires_at: new Date(Date.now() + lifetime.hours * HOUR_MS).toISOString(),
  };
}

function InstallCommand({ created, onDone }: InstallCommandProps) {
  const { t, language } = useI18n();
  const [copied, setCopied] = useState(false);
  const command = installCommand(window.location.origin, created.token);
  const until = stamp(language, created.expires_at);
  const copy = async () => {
    await navigator.clipboard.writeText(command);
    setCopied(true);
  };
  return (
    <Callout.Root color="green" size="2">
      <Callout.Icon>
        <TerminalSquare size={16} />
      </Callout.Icon>
      <Flex direction="column" gap="3">
        <Callout.Text>{t('poolPage.token.command')}</Callout.Text>
        <Code size="2" variant="outline" className="command" aria-label={t('poolPage.token.label')}>
          {command}
        </Code>
        <Text size="2">
          {created.reusable
            ? t('poolPage.token.many', { time: until })
            : t('poolPage.token.once', { time: until })}
        </Text>
        <Flex gap="2">
          {navigator.clipboard === undefined ? null : (
            <Button variant="soft" onClick={() => void copy()}>
              <Copy size={16} aria-hidden />
              {copied ? t('poolPage.token.copied') : t('poolPage.token.copy')}
            </Button>
          )}
          <Button variant="soft" color="gray" onClick={onDone}>
            {t('poolPage.token.done')}
          </Button>
        </Flex>
      </Flex>
    </Callout.Root>
  );
}

export function Registration({ pool }: RegistrationProps) {
  const { t } = useI18n();
  const api = useApi();
  const create = useMutation({
    mutationFn: (request: RegistrationTokenRequest) => api.registrationToken(request),
    gcTime: 0,
  });

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    create.mutate(requestOf(pool, new FormData(event.currentTarget)));
  };

  return (
    <Flex direction="column" gap="4">
      <Text as="p" size="2">
        {t('poolPage.token.explanation')}
      </Text>
      <form onSubmit={submit} aria-label={t('poolPage.token.title')}>
        <Flex gap="4" wrap="wrap" align="end">
          <SelectInput
            name="expires"
            label={t('poolPage.token.expires')}
            defaultValue={DAY.name}
            options={LIFETIMES.map((lifetime) => ({ value: lifetime.name, label: t(lifetime.key) }))}
          />
          <Flex pb="2">
            <CheckInput name="reusable" label={t('poolPage.token.reusable')} />
          </Flex>
          <Button type="submit" loading={create.isPending}>
            <KeyRound size={16} aria-hidden />
            {t('poolPage.token.create')}
          </Button>
        </Flex>
      </form>
      {create.isError ? <Refusal error={create.error} /> : null}
      {create.data === undefined ? null : (
        <InstallCommand created={create.data} onDone={() => create.reset()} />
      )}
    </Flex>
  );
}
