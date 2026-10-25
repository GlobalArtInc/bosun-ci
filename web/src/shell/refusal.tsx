import { Callout, Code, Flex, Text } from '@radix-ui/themes';
import { CircleAlert } from 'lucide-react';
import { ApiError } from '../api/client';
import { useI18n } from '../i18n/i18n';

interface RefusalProps {
  error: Error;
}

export function Refusal({ error }: RefusalProps) {
  const { t } = useI18n();
  const fields = error instanceof ApiError ? error.body.fields : [];
  return (
    <Callout.Root color="red" role="alert" size="1">
      <Callout.Icon>
        <CircleAlert size={16} />
      </Callout.Icon>
      <Flex direction="column" gap="1">
        <Callout.Text>{t('form.refused', { message: error.message })}</Callout.Text>
        {fields.map((fault) => (
          <Text key={fault.field} as="p" size="2">
            <Code variant="ghost">{fault.field}</Code> {fault.message}
          </Text>
        ))}
      </Flex>
    </Callout.Root>
  );
}
