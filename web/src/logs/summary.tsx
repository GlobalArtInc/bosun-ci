import { Card, Flex, Heading, Text } from '@radix-ui/themes';
import { lazy, Suspense } from 'react';
import type { Step } from '../api/client';
import { useSummary } from '../api/queries';
import { useI18n } from '../i18n/i18n';
import { Failure, Loading } from '../shell/status';

interface SummariesProps {
  steps: Step[];
}

interface StepSummaryProps {
  step: Step;
}

const Markdown = lazy(() => import('./markdown').then((loaded) => ({ default: loaded.Markdown })));

function StepSummary({ step }: StepSummaryProps) {
  const summary = useSummary(step.execution_id, step.id);
  return (
    <Card size="2" asChild>
      <article aria-label={step.name}>
        <Flex direction="column" gap="2">
          <Text size="2" weight="medium">
            {step.name}
          </Text>
          {summary.isPending ? <Loading /> : null}
          {summary.isError ? <Failure error={summary.error} onRetry={() => summary.refetch()} /> : null}
          {summary.data === undefined ? null : (
            <Suspense fallback={<Loading />}>
              <Markdown text={summary.data} />
            </Suspense>
          )}
        </Flex>
      </article>
    </Card>
  );
}

export function Summaries({ steps }: SummariesProps) {
  const { t } = useI18n();
  const summarised = steps.filter((step) => step.summary_size !== null);
  if (summarised.length === 0) {
    return null;
  }
  return (
    <Flex direction="column" gap="3">
      <Heading as="h4" size="3">
        {t('runPage.summaries.title')}
      </Heading>
      {summarised.map((step) => (
        <StepSummary key={step.id} step={step} />
      ))}
    </Flex>
  );
}
