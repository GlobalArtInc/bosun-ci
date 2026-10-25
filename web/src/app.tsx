import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { useState } from 'react';
import { type createBrowserRouter, type RouteObject, RouterProvider } from 'react-router';
import type { Api } from './api/client';
import { ApiProvider } from './api/context';
import { I18nProvider } from './i18n/i18n';
import { HomePage } from './pages/home';
import { NotFoundPage } from './pages/not-found';
import { PasswordPage } from './pages/password';
import { SignInPage } from './pages/sign-in';
import { Shell } from './shell/shell';
import { ThemeProvider } from './theme/theme';

interface AppProps {
  api: Api;
  router: ReturnType<typeof createBrowserRouter>;
}

export const routes: RouteObject[] = [
  { path: '/sign-in', element: <SignInPage /> },
  { path: '/password', element: <PasswordPage /> },
  {
    path: '/',
    element: <Shell />,
    children: [
      { index: true, element: <HomePage /> },
      { path: '*', element: <NotFoundPage /> },
    ],
  },
];

function createQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, refetchOnWindowFocus: false },
      mutations: { retry: false },
    },
  });
}

export function App({ api, router }: AppProps) {
  const [queryClient] = useState(createQueryClient);
  return (
    <ApiProvider api={api}>
      <QueryClientProvider client={queryClient}>
        <I18nProvider>
          <ThemeProvider>
            <RouterProvider router={router} />
          </ThemeProvider>
        </I18nProvider>
      </QueryClientProvider>
    </ApiProvider>
  );
}
