import { Heading } from '@radix-ui/themes';
import { useI18n } from '../i18n/i18n';

export function HomePage() {
  const { t } = useI18n();
  return (
    <Heading as="h1" size="7">
      {t('home.title')}
    </Heading>
  );
}
