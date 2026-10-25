import { describe, expect, it } from 'vitest';
import { type Message, messages } from './sse';

function body(chunks: string[]): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  return new ReadableStream({
    start(controller) {
      for (const chunk of chunks) {
        controller.enqueue(encoder.encode(chunk));
      }
      controller.close();
    },
  });
}

async function read(chunks: string[]): Promise<Message[]> {
  const read: Message[] = [];
  for await (const message of messages(body(chunks))) {
    read.push(message);
  }
  return read;
}

describe('the event stream parser', () => {
  it('reads the position, the reconnection time and the data of each message', async () => {
    expect(
      await read(['id: 7.7.3.3\nretry: 1000\n\n', ': keep-alive\n\n', 'id: 8.8.3.3\ndata: {"seq":8}\n\n']),
    ).toEqual([
      { id: '7.7.3.3', event: null, data: null, retry: 1000 },
      { id: '8.8.3.3', event: null, data: '{"seq":8}', retry: null },
    ]);
  });

  it('joins lines of data and messages split across chunks and line endings', async () => {
    expect(await read(['data: first\r', '\ndata:second\r\n\r', '\nid: 9.9.0.0\rdata: Мир\r\r'])).toEqual([
      { id: null, event: null, data: 'first\nsecond', retry: null },
      { id: '9.9.0.0', event: null, data: 'Мир', retry: null },
    ]);
  });

  it('keeps a character split across chunks whole', async () => {
    const encoded = new TextEncoder().encode('data: Мир\n\n');
    const split = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(encoded.slice(0, 7));
        controller.enqueue(encoded.slice(7));
        controller.close();
      },
    });
    const read: Message[] = [];
    for await (const message of messages(split)) {
      read.push(message);
    }
    expect(read).toEqual([{ id: null, event: null, data: 'Мир', retry: null }]);
  });

  it('ignores fields it does not know, a malformed retry and a message left unfinished', async () => {
    expect(await read(['event: other\nretry: soon\nfield\n\n', 'data: never dispatched\n'])).toEqual([]);
  });

  it('names the event of a message that carries a name', async () => {
    expect(await read(['id: 3\ndata: {"number":3}\n\n', 'event: end\ndata: {"lines":3}\n\n'])).toEqual([
      { id: '3', event: null, data: '{"number":3}', retry: null },
      { id: null, event: 'end', data: '{"lines":3}', retry: null },
    ]);
  });
});
