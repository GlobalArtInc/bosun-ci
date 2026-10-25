import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import execution from '../../../contracts/fixtures/api/execution.json';
import me from '../../../contracts/fixtures/api/me.json';
import jobs from '../../../contracts/fixtures/api/page-jobs.json';
import project from '../../../contracts/fixtures/api/project.json';
import run from '../../../contracts/fixtures/api/run.json';
import type { Annotation, Execution, LogLineData, Run, RunEvent, Step } from '../api/client';
import { translate } from '../i18n/i18n';
import { json, ORIGIN, renderAt } from '../test/render';

interface Followed {
  push: (text: string) => void;
  close: () => void;
}

interface Served {
  steps: Step[];
  later?: Step[];
  logs?: Record<string, string[]>;
  follow?: (request: Request, opened: Followed) => void;
  search?: LogLineData[];
  summaries?: Record<string, string>;
  events?: RunEvent[];
}

interface Hacked {
  hacked?: boolean;
}

const BUILD = jobs.items[0]?.id ?? '';

const TIME = '2026-10-24T18:34:51.123456789Z';

const ROW_PX = 20;

const encoder = new TextEncoder();

function step(index: number, name: string, changes: Partial<Step> = {}): Step {
  return {
    ...(execution.steps[0] as Step),
    id: `0192f3a4-5b6c-7d8e-9f01-0000000003${index.toString().padStart(2, '0')}`,
    index,
    name,
    status: 'completed',
    outcome: 'success',
    conclusion: 'success',
    conclusion_source: 'natural',
    annotations: [],
    summary_size: null,
    ...changes,
  };
}

function annotation(
  kind: Annotation['kind'],
  message: string,
  changes: Partial<Annotation> = {},
): Annotation {
  return {
    kind,
    message,
    title: null,
    file: null,
    line: null,
    end_line: null,
    column: null,
    end_column: null,
    ...changes,
  };
}

function body(lines: string[]): string {
  return lines.map((text) => `${TIME} ${text}\n`).join('');
}

function path(request: Request): string {
  return `${request.method} ${request.url.slice(ORIGIN.length).split('?')[0]}`;
}

function stream(onOpen: (opened: Followed) => void, signal: AbortSignal): Response {
  const readable = new ReadableStream<Uint8Array>({
    start(controller) {
      onOpen({
        push: (text) => controller.enqueue(encoder.encode(text)),
        close: () => controller.close(),
      });
      signal.addEventListener('abort', () => controller.error(signal.reason));
    },
  });
  return new Response(readable, { headers: { 'Content-Type': 'text/event-stream' } });
}

function serve(served: Served, requests: Request[] = []) {
  const shown: Run = { ...(run as Run), notices: [] };
  let reads = 0;
  let release = () => undefined;
  return (request: Request) => {
    requests.push(request.clone());
    const url = new URL(request.url);
    const where = path(request);
    if (where === 'GET /api/v1/me') {
      return json(200, me);
    }
    if (where === 'GET /api/v1/events') {
      return stream((opened) => {
        opened.push('id: 1.1.1.1\nretry: 1000\n\n');
        release = () => {
          for (const event of served.events ?? []) {
            opened.push(`id: ${event.seq}.${event.seq}.1.1\ndata: ${JSON.stringify(event)}\n\n`);
          }
          return undefined;
        };
      }, request.signal);
    }
    if (where === `GET /api/v1/runs/${shown.id}`) {
      return json(200, shown);
    }
    if (where === `GET /api/v1/projects/${project.id}`) {
      return json(200, project);
    }
    if (where === `GET /api/v1/runs/${shown.id}/attempts/1/jobs`) {
      return json(200, { items: jobs.items, next: null });
    }
    if (where === `GET /api/v1/executions/${execution.id}`) {
      reads += 1;
      const steps = reads > 1 && served.later !== undefined ? served.later : served.steps;
      setTimeout(release, 0);
      return json(200, { ...(execution as Execution), steps });
    }
    for (const known of served.steps) {
      const base = `GET /api/v1/executions/${execution.id}/steps/${known.id}`;
      if (
        where === `${base}/log` &&
        url.searchParams.get('follow') === 'true' &&
        served.follow !== undefined
      ) {
        const follow = served.follow;
        return stream((opened) => follow(request, opened), request.signal);
      }
      if (where === `${base}/log` && url.searchParams.has('search')) {
        return json(200, { matches: served.search ?? [] });
      }
      if (where === `${base}/log`) {
        return new Response(body(served.logs?.[known.id] ?? []), {
          headers: { 'Content-Type': 'text/plain; charset=utf-8' },
        });
      }
      if (where === `${base}/summary` && served.summaries?.[known.id] !== undefined) {
        return new Response(served.summaries[known.id], {
          headers: { 'Content-Type': 'text/markdown; charset=utf-8' },
        });
      }
    }
    return json(404, { error: { code: 'not_found', message: where, fields: [], diagnostics: [] } });
  };
}

function at(id: string): string {
  return `/runs/${run.id}?job=${BUILD}&step=${id}`;
}

function logOf(name: string): Promise<HTMLElement> {
  return screen.findByRole('region', { name: translate('en', 'runPage.log.label', { name }) });
}

function row(log: HTMLElement, number: number): HTMLElement | null {
  return log.querySelector(`[data-line="${number}"]`);
}

function scrollTo(element: HTMLElement, top: number) {
  Object.defineProperty(element, 'scrollTop', { configurable: true, value: top });
  fireEvent.scroll(element);
}

describe('the log viewer of the job panel', () => {
  it('renders only the visible lines of a log of 100 000 lines', async () => {
    const big = step(0, 'Print');
    const lines = Array.from({ length: 100_000 }, (_, index) => `line ${index + 1}`);
    renderAt(at(big.id), serve({ steps: [big], logs: { [big.id]: lines } }));

    const log = await logOf('Print');
    expect(await within(log).findByText('line 1')).toBeInTheDocument();
    expect(log.querySelectorAll('.log-row').length).toBeLessThan(100);
    expect(row(log, 100_000)).toBeNull();
    expect(log.querySelector('.log-space')).toHaveStyle({ height: `${100_000 * ROW_PX}px` });

    scrollTo(log, 99_990 * ROW_PX);

    await waitFor(() => expect(row(log, 100_000)).not.toBeNull());
    expect(row(log, 1)).toBeNull();
    expect(log.querySelectorAll('.log-row').length).toBeLessThan(100);
  });

  it('folds the lines of a group, and collapsing the group hides them again', async () => {
    const grouped = step(0, 'Install');
    renderAt(
      at(grouped.id),
      serve({
        steps: [grouped],
        logs: {
          [grouped.id]: [
            'before',
            '##[group]Install the packages',
            'inside one',
            'inside two',
            '##[endgroup]',
            'after',
          ],
        },
      }),
    );
    const user = userEvent.setup();

    const log = await logOf('Install');
    const toggle = await within(log).findByRole('button', { name: 'Install the packages' });
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(within(log).queryByText('inside one')).toBeNull();
    expect(within(log).getByText('after')).toBeInTheDocument();

    await user.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    expect(within(log).getByText('inside one')).toBeInTheDocument();
    expect(within(log).getByText('inside two')).toBeInTheDocument();
    expect(row(log, 5)).toBeNull();

    await user.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(within(log).queryByText('inside one')).toBeNull();
    expect(within(log).queryByText('inside two')).toBeNull();
  });

  it('colours ANSI sequences, and neither they nor the lines inject markup', async () => {
    const hostile = step(0, 'Hostile');
    renderAt(
      at(hostile.id),
      serve({
        steps: [hostile],
        logs: {
          [hostile.id]: [
            '\u001b[1;31mred\u001b[0m plain',
            '<script>window.hacked = true</script>',
            '\u001b]8;;javascript:window.hacked=true\u001b\\click\u001b]8;;\u001b\\',
            '\u001b[32m<img src=x onerror="window.hacked = true">\u001b[0m',
            '\u001b[38;2;1;2;3);background:url(x)mtext',
            '##[group]<b onclick="window.hacked = true">group</b>',
            '##[endgroup]',
            '##[error]<a href="javascript:window.hacked=true">error</a>',
          ],
        },
      }),
    );

    const log = await logOf('Hostile');
    const red = await within(log).findByText('red');
    expect(red).toHaveClass('ansi-fg-1', 'ansi-bold');
    expect(within(log).getByText('<script>window.hacked = true</script>')).toBeInTheDocument();
    expect(within(log).getByText('click')).toBeInTheDocument();
    expect(within(log).getByText('<img src=x onerror="window.hacked = true">')).toHaveClass('ansi-fg-2');
    expect(row(log, 6)).toHaveTextContent('<b onclick="window.hacked = true">group</b>');
    expect(row(log, 8)).toHaveTextContent('<a href="javascript:window.hacked=true">error</a>');
    expect(log.querySelector('script, img, b, iframe, [onclick], [onerror], [style*="url"]')).toBeNull();
    for (const anchor of log.querySelectorAll('a')) {
      expect(anchor).toHaveClass('log-number');
      expect(anchor.getAttribute('href')).toMatch(/^\/runs\//);
    }
    expect((window as Hacked).hacked).toBeUndefined();
  });

  it('shows an annotation at its line, and lists the job annotations with a way to that line', async () => {
    const failed = step(0, 'Compile', {
      conclusion: 'failure',
      outcome: 'failure',
      annotations: [
        annotation('warning', 'unused import', { file: 'src/lib.rs', line: 3 }),
        annotation('error', 'expected `;`, found `}`', {
          title: 'rustc',
          file: 'crates/types/src/api.rs',
          line: 41,
          column: 9,
        }),
      ],
    });
    const { router } = renderAt(
      `/runs/${run.id}?job=${BUILD}`,
      serve({
        steps: [failed],
        logs: {
          [failed.id]: [
            'Compiling bosun-types',
            '##[error]a line that looks like an error',
            '##[warning]unused import',
            'crates/types/src/api.rs:41:9 \u001b[31merror\u001b[0m: expected `;`, found `}`',
            '##[error]crates/types/src/api.rs:41:9 error: expected `;`, found `}`',
          ],
        },
      }),
    );
    const user = userEvent.setup();

    const list = await screen.findByRole('list', { name: translate('en', 'runPage.annotations.title') });
    const [first, second] = within(list).getAllByRole('listitem');
    expect(first).toHaveTextContent(translate('en', 'runPage.notices.error'));
    expect(first).toHaveTextContent('crates/types/src/api.rs:41:9');
    expect(first).toHaveTextContent('rustc');
    expect(first).toHaveTextContent(translate('en', 'runPage.annotations.step', { name: 'Compile' }));
    expect(second).toHaveTextContent('src/lib.rs:3');

    await user.click(
      within(first as HTMLElement).getByRole('link', { name: translate('en', 'runPage.annotations.show') }),
    );

    const log = await logOf('Compile');
    const line = await waitFor(() => {
      const found = row(log, 5);
      expect(found).not.toBeNull();
      return found as HTMLElement;
    });
    expect(line).toHaveAttribute('aria-current', 'location');
    expect(line).toHaveAttribute('data-annotation', 'error');
    expect(line).toHaveTextContent(translate('en', 'runPage.notices.error'));
    expect(line).toHaveTextContent('rustc · crates/types/src/api.rs:41:9');
    expect(row(log, 2)).not.toHaveAttribute('data-annotation');
    expect(row(log, 3)).toHaveAttribute('data-annotation', 'warning');
    expect(router.state.location.search).toContain(`step=${failed.id}`);
    expect(router.state.location.search).toContain('annotation=1');
  });

  it('follows a running step, appends its lines as they arrive and resumes after the last one', async () => {
    const running = step(0, 'Count', {
      status: 'in_progress',
      outcome: null,
      conclusion: null,
      conclusion_source: null,
      completed_at: null,
    });
    const opened: Followed[] = [];
    const resumed: (string | null)[] = [];
    renderAt(
      at(running.id),
      serve({
        steps: [running],
        follow: (request, stream) => {
          resumed.push(request.headers.get('Last-Event-ID'));
          opened.push(stream);
        },
      }),
    );

    await waitFor(() => expect(opened).toHaveLength(1));
    expect(
      screen.queryByRole('region', { name: translate('en', 'runPage.log.label', { name: 'Count' }) }),
    ).toBeNull();
    opened[0]?.push('retry: 10\n\n');
    for (const number of [1, 2]) {
      opened[0]?.push(
        `id: ${number}\ndata: ${JSON.stringify({ number, line: `${TIME} line ${number}` })}\n\n`,
      );
    }
    const followed = await logOf('Count');
    expect(await within(followed).findByText('line 2')).toBeInTheDocument();
    expect(screen.getByText(translate('en', 'runPage.log.following'))).toBeInTheDocument();

    opened[0]?.close();
    await waitFor(() => expect(opened).toHaveLength(2));
    opened[1]?.push(`id: 2\ndata: ${JSON.stringify({ number: 2, line: `${TIME} line 2` })}\n\n`);
    opened[1]?.push(`id: 3\ndata: ${JSON.stringify({ number: 3, line: `${TIME} line 3` })}\n\n`);
    opened[1]?.push(`event: end\ndata: ${JSON.stringify({ lines: 3 })}\n\n`);

    expect(await within(followed).findByText('line 3')).toBeInTheDocument();
    expect(within(followed).getAllByText('line 2')).toHaveLength(1);
    await waitFor(() => expect(screen.queryByText(translate('en', 'runPage.log.following'))).toBeNull());
    expect(resumed).toEqual([null, '2']);
  });

  it('searches the step through the server and walks from match to match', async () => {
    const searched = step(0, 'Grep');
    const lines = Array.from({ length: 30 }, (_, index) => `row ${index + 1}`);
    const requests: Request[] = [];
    renderAt(
      at(searched.id),
      serve(
        {
          steps: [searched],
          logs: { [searched.id]: lines },
          search: [
            { number: 5, line: `${TIME} row 5` },
            { number: 25, line: `${TIME} row 25` },
          ],
        },
        requests,
      ),
    );
    const user = userEvent.setup();

    const log = await logOf('Grep');
    await within(log).findByText('row 1');
    await user.type(screen.getByRole('textbox', { name: translate('en', 'runPage.log.search') }), 'ROW 5');
    await user.click(screen.getByRole('button', { name: translate('en', 'runPage.log.searchSubmit') }));

    expect(
      await screen.findByText(translate('en', 'runPage.log.matches', { current: 1, count: 2 })),
    ).toBeVisible();
    const sent = requests.find((request) => new URL(request.url).searchParams.has('search'));
    expect(new URL(sent?.url ?? ORIGIN).searchParams.get('search')).toBe('ROW 5');
    await waitFor(() => expect(row(log, 5)).toHaveAttribute('aria-current', 'location'));
    expect(row(log, 25)).toHaveClass('log-match');

    await user.click(screen.getByRole('button', { name: translate('en', 'runPage.log.nextMatch') }));
    expect(
      await screen.findByText(translate('en', 'runPage.log.matches', { current: 2, count: 2 })),
    ).toBeVisible();
    await waitFor(() => expect(row(log, 25)).toHaveAttribute('aria-current', 'location'));
    expect(row(log, 5)).toHaveClass('log-match');
  });

  it('shows the times of the lines on request, links to each line and offers the raw log', async () => {
    const timed = step(2, 'Timed');
    const { router } = renderAt(
      at(timed.id),
      serve({ steps: [timed], logs: { [timed.id]: ['one', 'two', 'three'] } }),
    );
    const user = userEvent.setup();

    const log = await logOf('Timed');
    await within(log).findByText('one');
    expect(log.querySelector('.log-time')).toBeNull();
    await user.click(screen.getByRole('checkbox', { name: translate('en', 'runPage.log.times') }));
    const time = row(log, 1)?.querySelector('.log-time');
    expect(time).toHaveAttribute('title', TIME);
    expect(time?.textContent).toMatch(/^\d\d:\d\d:\d\d.123$/);

    await user.click(
      within(log).getByRole('link', { name: translate('en', 'runPage.log.line', { number: 3 }) }),
    );
    expect(router.state.location.search).toContain('line=3');
    await waitFor(() => expect(row(log, 3)).toHaveAttribute('aria-current', 'location'));

    const download = screen.getByRole('link', { name: translate('en', 'runPage.log.download') });
    expect(download).toHaveAttribute('href', `/api/v1/executions/${execution.id}/steps/${timed.id}/log`);
    expect(download).toHaveAttribute('download', 'build-3.log');
  });

  it('renders the summaries of the job steps', async () => {
    const summarised = step(0, 'Report', { summary_size: 64 });
    renderAt(
      `/runs/${run.id}?job=${BUILD}`,
      serve({
        steps: [summarised, step(1, 'Quiet')],
        summaries: { [summarised.id]: '## 41 tests passed\n\n<script>window.hacked = true</script>' },
      }),
    );

    const summary = await screen.findByRole('article', { name: 'Report' });
    expect(await within(summary).findByRole('heading', { name: '41 tests passed' })).toBeInTheDocument();
    expect(summary.querySelector('script')).toBeNull();
    expect(screen.queryByRole('article', { name: 'Quiet' })).toBeNull();
  });

  it('reads the execution again when a step completes, for the annotations its event does not carry', async () => {
    const running = step(0, 'Lint', {
      status: 'in_progress',
      outcome: null,
      conclusion: null,
      conclusion_source: null,
      completed_at: null,
    });
    const done = step(0, 'Lint', {
      conclusion: 'failure',
      outcome: 'failure',
      annotations: [annotation('error', 'the value is never read')],
    });
    const completed: RunEvent = {
      seq: 40,
      run_id: run.id,
      attempt_id: jobs.items[0]?.attempt_id ?? null,
      job_id: BUILD,
      execution_id: execution.id,
      step_id: running.id,
      actor: { kind: 'system' },
      at: '2026-10-24T18:35:00Z',
      kind: 'step.status',
      data: {
        from: 'in_progress',
        to: 'completed',
        outcome: 'failure',
        conclusion: 'failure',
        conclusion_source: 'natural',
      },
    };
    renderAt(`/runs/${run.id}?job=${BUILD}`, serve({ steps: [running], later: [done], events: [completed] }));

    const list = await screen.findByRole('list', { name: translate('en', 'runPage.annotations.title') });
    expect(list).toHaveTextContent('the value is never read');
  });
});
