import { AlertDialog, Button, Callout, Flex, Text } from '@radix-ui/themes';
import { useMutation } from '@tanstack/react-query';
import { CircleAlert, OctagonX } from 'lucide-react';
import { useState } from 'react';
import type { CommandRequest, Job, Run } from '../api/client';
import { useApi } from '../api/context';
import { useI18n } from '../i18n/i18n';

interface CancelProps {
  run: Run;
  job?: Job;
}

const NO_TARGET = { attempt: null, job: null, step: null };

function commandOf(job: Job | undefined): CommandRequest {
  if (job === undefined) {
    return { type: 'run.cancel', target: NO_TARGET, reason: null };
  }
  return { type: 'job.cancel', target: { ...NO_TARGET, job: job.id }, reason: null };
}

export function Cancel({ run, job }: CancelProps) {
  const { t } = useI18n();
  const api = useApi();
  const [key] = useState(() => crypto.randomUUID());
  const cancel = useMutation({ mutationFn: () => api.command(run.id, commandOf(job), key) });
  const label = job === undefined ? t('runPage.cancel.run') : t('runPage.cancel.job');

  if (cancel.isSuccess) {
    return (
      <Text size="2" color="gray" role="status">
        {t('runPage.cancel.requested')}
      </Text>
    );
  }
  return (
    <Flex direction="column" gap="2" align="start">
      <AlertDialog.Root>
        <AlertDialog.Trigger>
          <Button color="red" variant="soft" loading={cancel.isPending}>
            <OctagonX size={16} aria-hidden />
            {label}
          </Button>
        </AlertDialog.Trigger>
        <AlertDialog.Content maxWidth="440px">
          <AlertDialog.Title>
            {job === undefined
              ? t('runPage.cancel.runQuestion', { name: `${run.display_name} #${run.number}` })
              : t('runPage.cancel.jobQuestion', { name: job.name })}
          </AlertDialog.Title>
          <AlertDialog.Description size="2">
            {job === undefined ? t('runPage.cancel.runEffect') : t('runPage.cancel.jobEffect')}
          </AlertDialog.Description>
          <Flex gap="3" mt="4" justify="end">
            <AlertDialog.Cancel>
              <Button variant="soft" color="gray">
                {t('runPage.cancel.keep')}
              </Button>
            </AlertDialog.Cancel>
            <AlertDialog.Action>
              <Button color="red" onClick={() => cancel.mutate()}>
                {label}
              </Button>
            </AlertDialog.Action>
          </Flex>
        </AlertDialog.Content>
      </AlertDialog.Root>
      {cancel.isError ? (
        <Callout.Root color="red" role="alert" size="1">
          <Callout.Icon>
            <CircleAlert size={16} />
          </Callout.Icon>
          <Callout.Text>{t('runPage.cancel.refused', { message: cancel.error.message })}</Callout.Text>
        </Callout.Root>
      ) : null}
    </Flex>
  );
}
