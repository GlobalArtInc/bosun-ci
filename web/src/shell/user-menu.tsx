import { Button, DropdownMenu } from '@radix-ui/themes';
import { CircleUser, LogOut } from 'lucide-react';
import type { Me } from '../api/client';
import { useI18n } from '../i18n/i18n';
import { useSignOut } from '../session/sign-out';

interface UserMenuProps {
  me: Me;
}

export function UserMenu({ me }: UserMenuProps) {
  const { t } = useI18n();
  const signOut = useSignOut();
  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger>
        <Button variant="ghost" color="gray">
          <CircleUser size={18} aria-hidden />
          {me.user.login}
          <DropdownMenu.TriggerIcon />
        </Button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Content align="end">
        <DropdownMenu.Label>{t('shell.user.signedInAs', { login: me.user.login })}</DropdownMenu.Label>
        <DropdownMenu.Separator />
        <DropdownMenu.Item disabled={signOut.isPending} onSelect={() => signOut.mutate()}>
          <LogOut size={16} aria-hidden />
          {t('shell.user.signOut')}
        </DropdownMenu.Item>
      </DropdownMenu.Content>
    </DropdownMenu.Root>
  );
}
