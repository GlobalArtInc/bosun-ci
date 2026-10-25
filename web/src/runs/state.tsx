import { Badge, Flex } from '@radix-ui/themes';
import {
  Ban,
  CircleCheck,
  CircleDashed,
  CircleMinus,
  CirclePause,
  CircleX,
  Clock,
  Hand,
  Hourglass,
  LoaderCircle,
  type LucideIcon,
  Timer,
  TriangleAlert,
} from 'lucide-react';
import type { ComponentProps } from 'react';
import type {
  ConclusionSource,
  JobStatus,
  RunConclusion,
  RunStatus,
  StepStatus,
  WaitingReason,
} from '../api/client';
import { type Key, useI18n } from '../i18n/i18n';

type Status = RunStatus | JobStatus | StepStatus;

type Color = ComponentProps<typeof Badge>['color'];

interface Look {
  key: Key;
  color: Color;
  icon: LucideIcon;
}

interface StateBadgeProps {
  status: Status;
  conclusion: RunConclusion | null;
  source?: ConclusionSource | null;
}

const STATUSES: Record<Status, Look> = {
  pending: { key: 'state.status.pending', color: 'gray', icon: CircleDashed },
  waiting: { key: 'state.status.waiting', color: 'amber', icon: Hourglass },
  queued: { key: 'state.status.queued', color: 'gray', icon: Clock },
  assigned: { key: 'state.status.assigned', color: 'blue', icon: LoaderCircle },
  in_progress: { key: 'state.status.in_progress', color: 'blue', icon: LoaderCircle },
  paused: { key: 'state.status.paused', color: 'orange', icon: CirclePause },
  held: { key: 'state.status.held', color: 'orange', icon: Hand },
  completed: { key: 'state.status.completed', color: 'gray', icon: CircleCheck },
};

const CONCLUSIONS: Record<RunConclusion, Look> = {
  success: { key: 'state.conclusion.success', color: 'green', icon: CircleCheck },
  failure: { key: 'state.conclusion.failure', color: 'red', icon: CircleX },
  cancelled: { key: 'state.conclusion.cancelled', color: 'gray', icon: Ban },
  skipped: { key: 'state.conclusion.skipped', color: 'gray', icon: CircleMinus },
  timed_out: { key: 'state.conclusion.timed_out', color: 'red', icon: Timer },
  startup_failure: { key: 'state.conclusion.startup_failure', color: 'red', icon: TriangleAlert },
};

export const WAITING: Record<WaitingReason, Key> = {
  untrusted: 'state.waiting.untrusted',
  approval: 'state.waiting.approval',
  wait_timer: 'state.waiting.wait_timer',
  policy: 'state.waiting.policy',
  concurrency: 'state.waiting.concurrency',
  max_parallel: 'state.waiting.max_parallel',
  paused: 'state.waiting.paused',
};

export const STATUS_FILTERS: Record<RunStatus | RunConclusion, Key> = {
  queued: STATUSES.queued.key,
  in_progress: STATUSES.in_progress.key,
  waiting: STATUSES.waiting.key,
  paused: STATUSES.paused.key,
  completed: STATUSES.completed.key,
  success: CONCLUSIONS.success.key,
  failure: CONCLUSIONS.failure.key,
  cancelled: CONCLUSIONS.cancelled.key,
  skipped: CONCLUSIONS.skipped.key,
  timed_out: CONCLUSIONS.timed_out.key,
  startup_failure: CONCLUSIONS.startup_failure.key,
};

export function StateBadge({ status, conclusion, source = null }: StateBadgeProps) {
  const { t } = useI18n();
  const look = status === 'completed' && conclusion !== null ? CONCLUSIONS[conclusion] : STATUSES[status];
  const Icon = look.icon;
  return (
    <Flex as="span" align="center" gap="2" wrap="wrap">
      <Badge color={look.color} size="2" variant="soft" highContrast>
        <Icon size={14} aria-hidden />
        {t(look.key)}
      </Badge>
      {source === 'operator' ? (
        <Badge color="violet" size="2" variant="outline" highContrast>
          {t('state.source.operator')}
        </Badge>
      ) : null}
    </Flex>
  );
}
