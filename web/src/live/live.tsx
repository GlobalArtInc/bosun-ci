import { type QueryClient, useQueryClient } from '@tanstack/react-query';
import { type ReactNode, useContext, useEffect, useMemo, useRef, useState } from 'react';
import { type Api, ApiError, type EventFilter } from '../api/client';
import { useApi } from '../api/context';
import { ME } from '../session/session';
import { type Live, LiveContext } from './context';
import { apply, type LiveEvent } from './patch';
import { FINAL, messages, pause, RECONNECT_MS } from './sse';

interface Position {
  last: string | null;
}

interface LiveProviderProps {
  children: ReactNode;
}

export async function stream(
  api: Api,
  queryClient: QueryClient,
  filter: EventFilter,
  position: Position,
  signal: AbortSignal,
  begun: () => void = () => undefined,
) {
  let wait = RECONNECT_MS;
  while (!signal.aborted) {
    try {
      const body = await api.events(filter, position.last, signal);
      for await (const message of messages(body)) {
        if (signal.aborted) {
          return;
        }
        if (message.retry !== null) {
          wait = message.retry;
        }
        if (message.id !== null) {
          position.last = message.id;
          begun();
        }
        if (message.data !== null) {
          apply(queryClient, JSON.parse(message.data) as LiveEvent);
        }
      }
    } catch (error) {
      if (signal.aborted) {
        return;
      }
      if (error instanceof ApiError && error.status === 401) {
        begun();
        void queryClient.invalidateQueries({ queryKey: ME });
        return;
      }
      if (error instanceof ApiError && FINAL.has(error.status)) {
        begun();
        return;
      }
    }
    begun();
    await pause(wait, signal);
  }
}

export function LiveProvider({ children }: LiveProviderProps) {
  const api = useApi();
  const queryClient = useQueryClient();
  const [wanted, setWanted] = useState<string | null>(null);
  const [ready, setReady] = useState(false);
  const position = useRef<Position>({ last: null });

  useEffect(() => {
    if (wanted === null) {
      return;
    }
    const controller = new AbortController();
    void stream(
      api,
      queryClient,
      JSON.parse(wanted) as EventFilter,
      position.current,
      controller.signal,
      () => setReady(true),
    );
    return () => controller.abort();
  }, [api, queryClient, wanted]);

  const value = useMemo<Live>(() => ({ ready, follow: setWanted }), [ready]);

  return <LiveContext.Provider value={value}>{children}</LiveContext.Provider>;
}

export function useLive(filter: EventFilter) {
  const { follow } = useContext(LiveContext);
  const wanted = JSON.stringify(filter);
  useEffect(() => {
    follow(wanted);
    return () => follow(null);
  }, [follow, wanted]);
}
