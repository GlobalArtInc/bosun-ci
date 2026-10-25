import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useState } from 'react';
import { type Api, ApiError, type LogLineData } from '../api/client';
import { useApi } from '../api/context';
import { FINAL, messages, pause, RECONNECT_MS } from '../live/sse';
import { ME } from '../session/session';
import { type LogLine, lineOf, linesOf } from './lines';

type LogState = 'loading' | 'following' | 'complete' | 'failed';

interface StepLog {
  lines: LogLine[];
  state: LogState;
  error: Error | null;
}

interface Sink {
  begun: () => void;
  line: (line: LogLine) => void;
  end: () => void;
  fail: (error: ApiError) => void;
}

const END = 'end';

const UNAUTHORISED = 401;

const FLUSH_MS = 100;

const LOADING: StepLog = { lines: [], state: 'loading', error: null };

function asError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

async function follow(api: Api, execution: string, step: string, signal: AbortSignal, sink: Sink) {
  let last: number | null = null;
  let wait = RECONNECT_MS;
  while (!signal.aborted) {
    try {
      const body = await api.followLog(execution, step, last, signal);
      sink.begun();
      for await (const message of messages(body)) {
        if (signal.aborted) {
          return;
        }
        if (message.retry !== null) {
          wait = message.retry;
        }
        if (message.data === null) {
          continue;
        }
        if (message.event === END) {
          sink.end();
          return;
        }
        const read = JSON.parse(message.data) as LogLineData;
        if (last === null || read.number > last) {
          last = read.number;
          sink.line(lineOf(read.number, read.line));
        }
      }
    } catch (error) {
      if (signal.aborted) {
        return;
      }
      if (error instanceof ApiError && (error.status === UNAUTHORISED || FINAL.has(error.status))) {
        sink.fail(error);
        return;
      }
    }
    await pause(wait, signal);
  }
}

export function useStepLog(execution: string, step: string, completed: boolean): StepLog {
  const api = useApi();
  const queryClient = useQueryClient();
  const [follows] = useState(!completed);
  const [log, setLog] = useState<StepLog>(LOADING);

  useEffect(() => {
    const controller = new AbortController();
    const { signal } = controller;
    if (!follows) {
      api.log(execution, step, signal).then(
        (body) => {
          if (!signal.aborted) {
            setLog({ lines: linesOf(body), state: 'complete', error: null });
          }
        },
        (error: unknown) => {
          if (!signal.aborted) {
            setLog({ lines: [], state: 'failed', error: asError(error) });
          }
        },
      );
      return () => controller.abort();
    }
    let pending: LogLine[] = [];
    let timer: ReturnType<typeof setTimeout> | null = null;
    const flush = (state: LogState, error: Error | null) => {
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
      const batch = pending;
      pending = [];
      setLog((previous) => ({
        lines: batch.length === 0 ? previous.lines : previous.lines.concat(batch),
        state,
        error,
      }));
    };
    void follow(api, execution, step, signal, {
      begun: () =>
        setLog((previous) => (previous.state === 'loading' ? { ...previous, state: 'following' } : previous)),
      line: (line) => {
        pending.push(line);
        if (timer === null) {
          timer = setTimeout(() => flush('following', null), FLUSH_MS);
        }
      },
      end: () => flush('complete', null),
      fail: (error) => {
        flush('failed', error);
        if (error.status === UNAUTHORISED) {
          void queryClient.invalidateQueries({ queryKey: ME });
        }
      },
    });
    return () => {
      controller.abort();
      if (timer !== null) {
        clearTimeout(timer);
      }
    };
  }, [api, queryClient, execution, step, follows]);

  return log;
}
