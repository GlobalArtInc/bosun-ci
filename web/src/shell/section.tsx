import { Card, Flex, Heading } from '@radix-ui/themes';
import type { ReactNode } from 'react';

interface SectionProps {
  title: string;
  children: ReactNode;
}

export function Section({ title, children }: SectionProps) {
  return (
    <Card size="2" asChild>
      <section aria-label={title}>
        <Flex direction="column" gap="3">
          <Heading as="h2" size="4">
            {title}
          </Heading>
          {children}
        </Flex>
      </section>
    </Card>
  );
}
