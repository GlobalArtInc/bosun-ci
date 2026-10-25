import { Button, Callout, Flex, Text } from '@radix-ui/themes';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { CircleAlert } from 'lucide-react';
import { type FormEvent, useState } from 'react';
import { Navigate, useLocation, useSearchParams } from 'react-router';
import { ApiError, type PasswordChangeRequest } from '../api/client';
import { useApi } from '../api/context';
import { type Key, useI18n } from '../i18n/i18n';
import { here, ME, safeNext, signInPath, useMe } from '../session/session';
import { useSignOut } from '../session/sign-out';
import { Failure, Loading } from '../shell/status';
import { AuthLayout } from './auth-layout';
import { Field } from './field';

const SHORTEST = 12;

type Fault = 'current' | 'new';

interface Problem {
  fault: Fault | null;
  key: Key;
  message?: string;
}

function check(current: string, password: string, repeated: string): Problem | null {
  if (password.length < SHORTEST) {
    return { fault: 'new', key: 'password.error.short' };
  }
  if (password !== repeated) {
    return { fault: 'new', key: 'password.error.mismatch' };
  }
  if (password === current) {
    return { fault: 'new', key: 'password.error.same' };
  }
  return null;
}

function refused(error: Error): Problem {
  if (error instanceof ApiError && error.field('current_password') !== undefined) {
    return { fault: 'current', key: 'password.error.current' };
  }
  return { fault: null, key: 'error.unexpected', message: error.message };
}

export function PasswordPage() {
  const { t } = useI18n();
  const api = useApi();
  const queryClient = useQueryClient();
  const location = useLocation();
  const [params] = useSearchParams();
  const next = safeNext(params.get('next'));
  const me = useMe();
  const signOut = useSignOut();
  const [problem, setProblem] = useState<Problem | null>(null);
  const change = useMutation({
    mutationFn: (request: PasswordChangeRequest) => api.changePassword(request),
    onSuccess: (changed) => queryClient.setQueryData(ME, changed),
    onError: (error) => setProblem(refused(error)),
  });

  if (me.isPending) {
    return <Loading />;
  }
  if (me.isError) {
    return <Failure error={me.error} onRetry={() => me.refetch()} />;
  }
  if (me.data === null) {
    return <Navigate to={signInPath(here(location))} replace />;
  }
  if (!me.data.password_change_required) {
    return <Navigate to={next} replace />;
  }

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const current = String(form.get('current_password') ?? '');
    const password = String(form.get('new_password') ?? '');
    const found = check(current, password, String(form.get('repeated_password') ?? ''));
    setProblem(found);
    if (found === null) {
      change.mutate({ current_password: current, new_password: password });
    }
  };

  return (
    <AuthLayout title={t('password.title')}>
      <Text size="3">{t('password.explanation')}</Text>
      {problem === null ? null : (
        <Callout.Root color="red" role="alert">
          <Callout.Icon>
            <CircleAlert size={16} />
          </Callout.Icon>
          <Callout.Text>{t(problem.key, { message: problem.message ?? '' })}</Callout.Text>
        </Callout.Root>
      )}
      <form onSubmit={submit}>
        <Flex direction="column" gap="4">
          <Field
            name="current_password"
            label={t('password.current')}
            type="password"
            autoComplete="current-password"
            invalid={problem?.fault === 'current'}
            autoFocus
          />
          <Field
            name="new_password"
            label={t('password.new')}
            type="password"
            autoComplete="new-password"
            hint={t('password.rule')}
            invalid={problem?.fault === 'new'}
          />
          <Field
            name="repeated_password"
            label={t('password.repeat')}
            type="password"
            autoComplete="new-password"
            invalid={problem?.fault === 'new'}
          />
          <Button type="submit" size="3" loading={change.isPending}>
            {t('password.submit')}
          </Button>
          <Button type="button" size="3" variant="ghost" color="gray" onClick={() => signOut.mutate()}>
            {t('shell.user.signOut')}
          </Button>
        </Flex>
      </form>
    </AuthLayout>
  );
}
