import { Badge, Code, Flex, Heading, Link, Text } from '@radix-ui/themes';
import { Link as RouterLink, useSearchParams } from 'react-router';
import type { Annotation, Step } from '../api/client';
import { useI18n } from '../i18n/i18n';
import { bySeverity, SEVERITIES } from '../runs/severity';
import { withAnnotation } from './address';
import { location } from './lines';

interface AnnotationsProps {
  steps: Step[];
}

interface Entry {
  step: Step;
  annotation: Annotation;
  index: number;
}

export function Annotations({ steps }: AnnotationsProps) {
  const { t } = useI18n();
  const [params] = useSearchParams();
  const entries = bySeverity(
    steps.flatMap((step) =>
      step.annotations.map((annotation, index): Entry => ({ step, annotation, index })),
    ),
    (entry) => entry.annotation.kind,
  );
  if (entries.length === 0) {
    return null;
  }
  return (
    <Flex direction="column" gap="2">
      <Heading as="h4" size="3">
        {t('runPage.annotations.title')}
      </Heading>
      <Flex asChild direction="column" gap="3" m="0" p="0">
        <ol className="plain-list" aria-label={t('runPage.annotations.title')}>
          {entries.map(({ step, annotation, index }) => {
            const severity = SEVERITIES[annotation.kind];
            const place = location(annotation);
            return (
              <li key={`${step.id}-${index}`}>
                <Flex direction="column" gap="1">
                  <Flex align="center" gap="2" wrap="wrap">
                    <Badge color={severity.color} size="2" highContrast>
                      {t(severity.key)}
                    </Badge>
                    {annotation.title === null ? null : (
                      <Text size="2" weight="medium">
                        {annotation.title}
                      </Text>
                    )}
                    {place === null ? null : (
                      <Code variant="ghost" size="2">
                        {place}
                      </Code>
                    )}
                  </Flex>
                  <Text as="p" size="2" className="annotation-message">
                    {annotation.message}
                  </Text>
                  <Flex align="center" gap="3" wrap="wrap">
                    <Text size="2" color="gray">
                      {t('runPage.annotations.step', { name: step.name })}
                    </Text>
                    <Link asChild size="2">
                      <RouterLink to={`?${withAnnotation(params, step.id, index).toString()}`}>
                        {t('runPage.annotations.show')}
                      </RouterLink>
                    </Link>
                  </Flex>
                </Flex>
              </li>
            );
          })}
        </ol>
      </Flex>
    </Flex>
  );
}
