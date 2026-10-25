import { ShieldCheck } from 'lucide-react';
import type { CommandRequest, Job, Run } from '../api/client';
import { useI18n } from '../i18n/i18n';
import { CommandButton, NO_TARGET } from './command';

interface AdmitProps {
  run: Run;
}

const ADMIT: CommandRequest = { type: 'run.admit', target: NO_TARGET, reason: null };

export function awaitsAdmission(run: Run, jobs: Job[]): boolean {
  return (
    run.status !== 'completed' &&
    jobs.some((job) => job.status === 'waiting' && job.waiting_reason === 'untrusted')
  );
}

export function Admit({ run }: AdmitProps) {
  const { t } = useI18n();
  return (
    <CommandButton
      run={run}
      command={ADMIT}
      color="green"
      icon={ShieldCheck}
      texts={{
        label: t('runPage.admit.run'),
        question: t('runPage.admit.question', { name: `${run.display_name} #${run.number}` }),
        effect: t('runPage.admit.effect'),
        keep: t('runPage.admit.keep'),
        requested: t('runPage.admit.requested'),
        refused: (message) => t('runPage.admit.refused', { message }),
      }}
    />
  );
}
