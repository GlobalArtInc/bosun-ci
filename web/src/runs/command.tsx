import { AlertDialog, Button, Callout, Flex, Text } from '@radix-ui/themes';
import { useMutation } from '@tanstack/react-query';
import { CircleAlert, type LucideIcon } from 'lucide-react';
import { useState } from 'react';
import type { CommandRequest, Run } from '../api/client';
import { useApi } from '../api/context';

interface CommandTexts {
  label: string;
  question: string;
  effect: string;
  keep: string;
  requested: string;
  refused: (message: string) => string;
}

interface CommandButtonProps {
  run: Run;
  command: CommandRequest;
  color: 'red' | 'green';
  icon: LucideIcon;
  texts: CommandTexts;
}

export const NO_TARGET = { attempt: null, job: null, step: null };

export function CommandButton({ run, command, color, icon: Icon, texts }: CommandButtonProps) {
  const api = useApi();
  const [key] = useState(() => crypto.randomUUID());
  const sent = useMutation({ mutationFn: () => api.command(run.id, command, key) });

  if (sent.isSuccess) {
    return (
      <Text size="2" color="gray" role="status">
        {texts.requested}
      </Text>
    );
  }
  return (
    <Flex direction="column" gap="2" align="start">
      <AlertDialog.Root>
        <AlertDialog.Trigger>
          <Button color={color} variant="soft" loading={sent.isPending}>
            <Icon size={16} aria-hidden />
            {texts.label}
          </Button>
        </AlertDialog.Trigger>
        <AlertDialog.Content maxWidth="440px">
          <AlertDialog.Title>{texts.question}</AlertDialog.Title>
          <AlertDialog.Description size="2">{texts.effect}</AlertDialog.Description>
          <Flex gap="3" mt="4" justify="end">
            <AlertDialog.Cancel>
              <Button variant="soft" color="gray">
                {texts.keep}
              </Button>
            </AlertDialog.Cancel>
            <AlertDialog.Action>
              <Button color={color} onClick={() => sent.mutate()}>
                {texts.label}
              </Button>
            </AlertDialog.Action>
          </Flex>
        </AlertDialog.Content>
      </AlertDialog.Root>
      {sent.isError ? (
        <Callout.Root color="red" role="alert" size="1">
          <Callout.Icon>
            <CircleAlert size={16} />
          </Callout.Icon>
          <Callout.Text>{texts.refused(sent.error.message)}</Callout.Text>
        </Callout.Root>
      ) : null}
    </Flex>
  );
}
