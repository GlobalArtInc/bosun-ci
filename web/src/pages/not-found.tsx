import { Flex, Heading, Link, Text } from '@radix-ui/themes';
import { Link as RouterLink, useLocation } from 'react-router';
import { useI18n } from '../i18n/i18n';

export function NotFoundPage() {
  const { t } = useI18n();
  const location = useLocation();
  return (
    <Flex direction="column" gap="3">
      <Heading as="h1" size="7">
        {t('notFound.title')}
      </Heading>
      <Text size="3">{t('notFound.message', { path: location.pathname })}</Text>
      <Text size="3">
        <Link asChild>
          <RouterLink to="/">{t('notFound.home')}</RouterLink>
        </Link>
      </Text>
    </Flex>
  );
}
