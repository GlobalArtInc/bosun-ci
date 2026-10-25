import { Box, Card, Flex, Heading } from '@radix-ui/themes';
import type { ReactNode } from 'react';
import { Wordmark } from '../shell/shell';
import { Switches } from '../shell/switches';

interface AuthLayoutProps {
  title: string;
  children: ReactNode;
}

export function AuthLayout({ title, children }: AuthLayoutProps) {
  return (
    <Flex direction="column" minHeight="100vh">
      <Flex asChild align="center" justify="between" px="5" py="3">
        <header>
          <Wordmark />
          <Switches />
        </header>
      </Flex>
      <Flex asChild flexGrow="1" align="center" justify="center" px="4" pb="9">
        <main>
          <Box width="100%" maxWidth="400px">
            <Card size="4">
              <Flex direction="column" gap="5">
                <Heading as="h1" size="6">
                  {title}
                </Heading>
                {children}
              </Flex>
            </Card>
          </Box>
        </main>
      </Flex>
    </Flex>
  );
}
