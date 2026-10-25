import { type QueryClient, useQuery } from '@tanstack/react-query';
import type { Location } from 'react-router';
import { type Api, ApiError, type Me } from '../api/client';
import { useApi } from '../api/context';

export const ME = ['me'] as const;

const SIGN_IN = '/sign-in';

const PASSWORD = '/password';

async function readMe(api: Api): Promise<Me | null> {
  try {
    return await api.me();
  } catch (error) {
    if (error instanceof ApiError && error.status === 401) {
      return null;
    }
    throw error;
  }
}

export function useMe() {
  const api = useApi();
  return useQuery({ queryKey: ME, queryFn: () => readMe(api) });
}

export function forgetSession(queryClient: QueryClient) {
  queryClient.removeQueries({ predicate: (query) => query.queryKey[0] !== ME[0] });
  queryClient.setQueryData(ME, null);
}

export function safeNext(value: string | null): string {
  if (value === null || !value.startsWith('/') || value.startsWith('//') || value.startsWith('/\\')) {
    return '/';
  }
  return value;
}

function withNext(path: string, next: string): string {
  return next === '/' ? path : `${path}?${new URLSearchParams({ next })}`;
}

export function here(location: Location): string {
  return `${location.pathname}${location.search}${location.hash}`;
}

export function signInPath(next: string): string {
  return withNext(SIGN_IN, next);
}

export function passwordPath(next: string): string {
  return withNext(PASSWORD, next);
}
