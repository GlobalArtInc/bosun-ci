export interface Message {
  id: string | null;
  event: string | null;
  data: string | null;
  retry: number | null;
}

interface Fields {
  id: string | null;
  event: string | null;
  data: string[];
  retry: number | null;
}

export const RECONNECT_MS = 1000;

export const FINAL = new Set([403, 404, 422]);

function fresh(): Fields {
  return { id: null, event: null, data: [], retry: null };
}

export function pause(milliseconds: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, milliseconds);
    signal.addEventListener(
      'abort',
      () => {
        clearTimeout(timer);
        resolve();
      },
      { once: true },
    );
  });
}

function field(fields: Fields, line: string) {
  const colon = line.indexOf(':');
  const name = colon === -1 ? line : line.slice(0, colon);
  const raw = colon === -1 ? '' : line.slice(colon + 1);
  const value = raw.startsWith(' ') ? raw.slice(1) : raw;
  if (name === 'data') {
    fields.data.push(value);
  } else if (name === 'event') {
    fields.event = value;
  } else if (name === 'id' && !value.includes('\0')) {
    fields.id = value;
  } else if (name === 'retry' && /^\d+$/.test(value)) {
    fields.retry = Number(value);
  }
}

function complete(fields: Fields): Message | null {
  if (fields.id === null && fields.data.length === 0 && fields.retry === null) {
    return null;
  }
  return {
    id: fields.id,
    event: fields.event,
    data: fields.data.length === 0 ? null : fields.data.join('\n'),
    retry: fields.retry,
  };
}

export async function* messages(body: ReadableStream<Uint8Array>): AsyncGenerator<Message> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';
  let fields = fresh();
  let ended = false;
  try {
    while (!ended) {
      const read = await reader.read();
      ended = read.done;
      buffer += ended ? decoder.decode() : decoder.decode(read.value, { stream: true });
      for (;;) {
        const end = buffer.search(/[\r\n]/);
        if (end === -1 || (buffer[end] === '\r' && end === buffer.length - 1 && !ended)) {
          break;
        }
        const line = buffer.slice(0, end);
        buffer = buffer.slice(buffer.startsWith('\r\n', end) ? end + 2 : end + 1);
        if (line === '') {
          const message = complete(fields);
          fields = fresh();
          if (message !== null) {
            yield message;
          }
        } else if (!line.startsWith(':')) {
          field(fields, line);
        }
      }
    }
  } finally {
    reader.releaseLock();
  }
}
