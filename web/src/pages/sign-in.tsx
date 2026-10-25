import { Button, Callout, Flex, Separator, Text } from '@radix-ui/themes';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { CircleAlert } from 'lucide-react';
import type { FormEvent } from 'react';
import { Navigate, useSearchParams } from 'react-router';
import { ApiError, type SignInRequest } from '../api/client';
import { useApi } from '../api/context';
import { type I18n, useI18n } from '../i18n/i18n';
import { ME, passwordPath, safeNext, useMe } from '../session/session';
import { Failure, Loading } from '../shell/status';
import { AuthLayout } from './auth-layout';
import { Field } from './field';

const METHODS = ['auth', 'methods'] as const;

function refusal(error: Error, t: I18n['t']): string {
  if (error instanceof ApiError && error.status === 401) {
    return t('signIn.error.wrong');
  }
  if (error instanceof ApiError && error.code === 'rate_limited') {
    return t('signIn.error.locked', { seconds: error.retryAfter ?? 60 });
  }
  return t('error.unexpected', { message: error.message });
}

export function SignInPage() {
  const { t } = useI18n();
  const api = useApi();
  const queryClient = useQueryClient();
  const [params] = useSearchParams();
  const next = safeNext(params.get('next'));
  const me = useMe();
  const methods = useQuery({ queryKey: METHODS, queryFn: () => api.methods() });
  const signIn = useMutation({
    mutationFn: (request: SignInRequest) => api.signIn(request),
    onSuccess: (signed) => queryClient.setQueryData(ME, signed),
  });

  if (me.data) {
    return <Navigate to={me.data.password_change_required ? passwordPath(next) : next} replace />;
  }

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    signIn.mutate({ login: String(form.get('login') ?? ''), password: String(form.get('password') ?? '') });
  };

  return (
    <AuthLayout title={t('signIn.title')}>
      {methods.isPending ? <Loading /> : null}
      {methods.isError ? <Failure error={methods.error} onRetry={() => methods.refetch()} /> : null}
      {signIn.isError ? (
        <Callout.Root color="red" role="alert">
          <Callout.Icon>
            <CircleAlert size={16} />
          </Callout.Icon>
          <Callout.Text>{refusal(signIn.error, t)}</Callout.Text>
        </Callout.Root>
      ) : null}
      {methods.data?.password ? (
        <form onSubmit={submit}>
          <Flex direction="column" gap="4">
            <Field name="login" label={t('signIn.login')} type="text" autoComplete="username" autoFocus />
            <Field
              name="password"
              label={t('signIn.password')}
              type="password"
              autoComplete="current-password"
            />
            <Button type="submit" size="3" loading={signIn.isPending}>
              {t('signIn.submit')}
            </Button>
          </Flex>
        </form>
      ) : null}
      {methods.data?.password && methods.data.providers.length > 0 ? (
        <Flex align="center" gap="3">
          <Separator size="4" />
          <Text size="2" color="gray">
            {t('signIn.or')}
          </Text>
          <Separator size="4" />
        </Flex>
      ) : null}
      {methods.data && methods.data.providers.length > 0 ? (
        <Flex direction="column" gap="3">
          {methods.data.providers.map((provider) => (
            <Button key={provider.name} asChild size="3" variant="soft">
              <a href={`/api/v1/auth/oidc/${encodeURIComponent(provider.name)}/start`}>
                {t('signIn.provider', { name: provider.name })}
              </a>
            </Button>
          ))}
        </Flex>
      ) : null}
      {methods.data && !methods.data.password && methods.data.providers.length === 0 ? (
        <Text size="3">{t('signIn.noMethods')}</Text>
      ) : null}
    </AuthLayout>
  );
}
