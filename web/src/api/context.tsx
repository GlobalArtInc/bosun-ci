import { createContext, type ReactNode, useContext } from 'react';
import type { Api } from './client';

interface ApiProviderProps {
  api: Api;
  children: ReactNode;
}

const ApiContext = createContext<Api | null>(null);

export function ApiProvider({ api, children }: ApiProviderProps) {
  return <ApiContext.Provider value={api}>{children}</ApiContext.Provider>;
}

export function useApi(): Api {
  const api = useContext(ApiContext);
  if (api === null) {
    throw new Error('useApi is used outside ApiProvider');
  }
  return api;
}
