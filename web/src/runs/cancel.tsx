import { OctagonX } from 'lucide-react';
import type { CommandRequest, Job, Run } from '../api/client';
import { useI18n } from '../i18n/i18n';
import { CommandButton, NO_TARGET } from './command';

interface CancelProps {
  run: Run;
  job?: Job;
}

function commandOf(job: Job | undefined): CommandRequest {
  if (job === undefined) {
    return { type: 'run.cancel', target: NO_TARGET, reason: null };
  }
  return { type: 'job.cancel', target: { ...NO_TARGET, job: job.id }, reason: null };
}

export function Cancel({ run, job }: CancelProps) {
  const { t } = useI18n();
  return (
    <CommandButton
      run={run}
      command={commandOf(job)}
      color="red"
      icon={OctagonX}
      texts={{
        label: job === undefined ? t('runPage.cancel.run') : t('runPage.cancel.job'),
        question:
          job === undefined
            ? t('runPage.cancel.runQuestion', { name: `${run.display_name} #${run.number}` })
            : t('runPage.cancel.jobQuestion', { name: job.name }),
        effect: job === undefined ? t('runPage.cancel.runEffect') : t('runPage.cancel.jobEffect'),
        keep: t('runPage.cancel.keep'),
        requested: t('runPage.cancel.requested'),
        refused: (message) => t('runPage.cancel.refused', { message }),
      }}
    />
  );
}
