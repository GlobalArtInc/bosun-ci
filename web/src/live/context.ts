import { createContext, useContext } from 'react';

export interface Live {
  ready: boolean;
  follow: (filter: string | null) => void;
}

export const LiveContext = createContext<Live>({ ready: true, follow: () => undefined });

export function useLiveReady(): boolean {
  return useContext(LiveContext).ready;
}
