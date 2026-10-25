import { Button, Callout, Flex, Spinner } from '@radix-ui/themes';
import { CircleAlert } from 'lucide-react';
import { useI18n } from '../i18n/i18n';

interface FailureProps {
  error: Error;
  onRetry?: () => void;
}

export function Loading() {
  const { t } = useI18n();
  return (
    <Flex role="status" aria-label={t('app.loading')} align="center" justify="center" p="9">
      <Spinner size="3" />
    </Flex>
  );
}

export function Failure({ error, onRetry }: FailureProps) {
  const { t } = useI18n();
  return (
    <Callout.Root color="red" role="alert">
      <Callout.Icon>
        <CircleAlert size={16} />
      </Callout.Icon>
      <Callout.Text>{t('error.unexpected', { message: error.message })}</Callout.Text>
      {onRetry === undefined ? null : (
        <Flex>
          <Button variant="soft" color="red" onClick={onRetry}>
            {t('error.retry')}
          </Button>
        </Flex>
      )}
    </Callout.Root>
  );
}
