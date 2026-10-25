import { Button, Checkbox, Flex, IconButton, Spinner, Text, TextField } from '@radix-ui/themes';
import { type UseMutationResult, useMutation } from '@tanstack/react-query';
import { ChevronDown, ChevronRight, ChevronUp, Download, Search } from 'lucide-react';
import { type FormEvent, type UIEvent, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { Link as RouterLink, useSearchParams } from 'react-router';
import type { Annotation, Job, LogLineData, Step } from '../api/client';
import { useApi } from '../api/context';
import { type Key, useI18n } from '../i18n/i18n';
import { Failure, Loading } from '../shell/status';
import { withLine } from './address';
import { AnsiText } from './ansi-text';
import { useStepLog } from './follow';
import {
  type Group,
  indexOfLine,
  type LogLine,
  location,
  type Marker,
  marked,
  placeAnnotations,
  rowAtOrBefore,
  rowsOf,
  type Structure,
  structure,
} from './lines';

interface LogViewerProps {
  job: Job;
  step: Step;
  times: boolean;
  onTimes: (shown: boolean) => void;
  line: number | null;
  annotation: number | null;
}

interface SearchProps {
  search: UseMutationResult<LogLineData[], Error, string>;
  line: number | null;
  onGo: (line: number) => void;
}

interface LinesProps {
  step: Step;
  lines: LogLine[];
  times: boolean;
  following: boolean;
  line: number | null;
  annotation: number | null;
  matches: Set<number>;
}

interface RowProps {
  line: LogLine;
  marker: Marker | null;
  times: boolean;
  clock: (time: string) => string;
  href: string;
  current: boolean;
  match: boolean;
  annotation: Annotation | null;
  group: GroupToggle | null;
}

interface GroupToggle {
  open: boolean;
  onToggle: () => void;
}

interface Toggles {
  opened: ReadonlySet<number>;
  closed: ReadonlySet<number>;
}

const ROW_PX = 20;

const VIEWPORT_PX = 480;

const OVERSCAN = 20;

const NONE = -1;

const MARKER_KEYS: Record<'error' | 'warning' | 'notice' | 'debug', Key> = {
  error: 'runPage.notices.error',
  warning: 'runPage.notices.warning',
  notice: 'runPage.notices.notice',
  debug: 'runPage.log.debug',
};

const FRACTION = /\.(\d+)/;

const NO_TOGGLES: Toggles = { opened: new Set(), closed: new Set() };

function isOpen(toggles: Toggles, group: Group): boolean {
  return toggles.opened.has(group.header) || (!toggles.closed.has(group.header) && !group.ended);
}

function toggled(toggles: Toggles, group: Group): Toggles {
  const opened = new Set(toggles.opened);
  const closed = new Set(toggles.closed);
  if (isOpen(toggles, group)) {
    opened.delete(group.header);
    closed.add(group.header);
  } else {
    closed.delete(group.header);
    opened.add(group.header);
  }
  return { opened, closed };
}

function useClock(language: string): (time: string) => string {
  return useMemo(() => {
    const format = new Intl.DateTimeFormat(language, {
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      fractionalSecondDigits: 3,
      hourCycle: 'h23',
    });
    return (time: string) => {
      const at = Date.parse(
        time.replace(FRACTION, (_, digits: string) => `.${digits.slice(0, 3).padEnd(3, '0')}`),
      );
      return Number.isNaN(at) ? time : format.format(at);
    };
  }, [language]);
}

function viewportOf(element: HTMLElement | null): number {
  return element === null || element.clientHeight === 0 ? VIEWPORT_PX : element.clientHeight;
}

function Row({ line, marker, times, clock, href, current, match, annotation, group }: RowProps) {
  const { t } = useI18n();
  const { rest } = marked(line.text);
  const classes = ['log-row'];
  if (current) {
    classes.push('log-current');
  } else if (match) {
    classes.push('log-match');
  }
  if (annotation !== null) {
    classes.push(`log-annotation-${annotation.kind}`);
  }
  const place =
    annotation === null ? null : [annotation.title, location(annotation)].filter(Boolean).join(' · ');
  return (
    <div
      className={classes.join(' ')}
      data-line={line.number}
      data-annotation={annotation?.kind}
      aria-current={current ? 'location' : undefined}
    >
      <RouterLink
        to={href}
        className="log-number"
        aria-label={t('runPage.log.line', { number: line.number })}
      >
        {line.number}
      </RouterLink>
      {times ? (
        <span className="log-time" title={line.time}>
          {clock(line.time)}
        </span>
      ) : null}
      <span className="log-text">
        {group !== null ? (
          <button type="button" className="log-toggle" aria-expanded={group.open} onClick={group.onToggle}>
            {group.open ? <ChevronDown size={14} aria-hidden /> : <ChevronRight size={14} aria-hidden />}
            <AnsiText text={rest} />
          </button>
        ) : marker === 'error' || marker === 'warning' || marker === 'notice' || marker === 'debug' ? (
          <>
            <span className={`log-marker log-marker-${marker}`}>{t(MARKER_KEYS[marker])}</span>
            <AnsiText text={rest} />
            {place === null || place === '' ? null : <span className="log-place">{place}</span>}
          </>
        ) : (
          <AnsiText text={line.text} />
        )}
      </span>
    </div>
  );
}

function targetOf(
  lines: LogLine[],
  placed: (number | null)[],
  line: number | null,
  annotation: number | null,
): number | null {
  if (line !== null) {
    return indexOfLine(lines, line);
  }
  if (annotation !== null) {
    return placed[annotation] ?? null;
  }
  return null;
}

function hiddenGroup(shape: Structure, toggles: Toggles, target: number | null): Group | null {
  if (target === null) {
    return null;
  }
  const header = shape.groupOf[target] ?? NONE;
  const group = header === NONE ? undefined : shape.groups.get(header);
  return group === undefined || isOpen(toggles, group) ? null : group;
}

function Lines({ step, lines, times, following, line, annotation, matches }: LinesProps) {
  const { t, language } = useI18n();
  const clock = useClock(language);
  const [params] = useSearchParams();
  const scroller = useRef<HTMLElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewport, setViewport] = useState(VIEWPORT_PX);
  const [toggles, setToggles] = useState<Toggles>(NO_TOGGLES);
  const stuck = useRef(following && line === null && annotation === null);
  const handled = useRef<number | null>(null);

  const shape = useMemo(() => structure(lines), [lines]);
  const placed = useMemo(
    () => placeAnnotations(lines, shape, step.annotations),
    [lines, shape, step.annotations],
  );
  const annotated = useMemo(() => {
    const at = new Map<number, Annotation>();
    placed.forEach((index, order) => {
      const found = step.annotations[order];
      if (index !== null && found !== undefined && !at.has(index)) {
        at.set(index, found);
      }
    });
    return at;
  }, [placed, step.annotations]);
  const rows = useMemo(() => rowsOf(shape, (group) => isOpen(toggles, group)), [shape, toggles]);
  const target = targetOf(lines, placed, line, annotation);
  const hidden = hiddenGroup(shape, toggles, target);

  useLayoutEffect(() => {
    const element = scroller.current;
    if (element === null) {
      return;
    }
    setViewport(viewportOf(element));
    const observer = new ResizeObserver(() => setViewport(viewportOf(element)));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  useLayoutEffect(() => {
    if (hidden !== null) {
      setToggles((previous) => toggled(previous, hidden));
    }
  }, [hidden]);

  useLayoutEffect(() => {
    if (target === null) {
      handled.current = null;
      return;
    }
    if (hidden !== null || handled.current === target) {
      return;
    }
    handled.current = target;
    stuck.current = false;
    const element = scroller.current;
    const shown = viewportOf(element);
    const top = rowAtOrBefore(rows, target) * ROW_PX;
    if (element !== null && (top < element.scrollTop || top + ROW_PX > element.scrollTop + shown)) {
      const next = Math.max(0, Math.min(rows.length * ROW_PX - shown, top - shown / 2 + ROW_PX / 2));
      element.scrollTop = next;
      setScrollTop(next);
    }
  }, [target, hidden, rows]);

  useLayoutEffect(() => {
    const element = scroller.current;
    if (!following || !stuck.current || element === null) {
      return;
    }
    const next = Math.max(0, rows.length * ROW_PX - viewportOf(element));
    element.scrollTop = next;
    setScrollTop(next);
  }, [following, rows.length]);

  const onScroll = (event: UIEvent<HTMLElement>) => {
    const element = event.currentTarget;
    setScrollTop(element.scrollTop);
    stuck.current = element.scrollTop + viewportOf(element) >= rows.length * ROW_PX - ROW_PX;
  };

  const first = Math.max(0, Math.floor(scrollTop / ROW_PX) - OVERSCAN);
  const last = Math.min(rows.length, Math.ceil((scrollTop + viewport) / ROW_PX) + OVERSCAN);

  return (
    <section
      ref={scroller}
      className="log-lines"
      aria-label={t('runPage.log.label', { name: step.name })}
      onScroll={onScroll}
    >
      <div className="log-space" style={{ height: rows.length * ROW_PX }}>
        <div className="log-window" style={{ transform: `translateY(${first * ROW_PX}px)` }}>
          {rows.slice(first, last).map((index) => {
            const shown = lines[index];
            if (shown === undefined) {
              return null;
            }
            const group = shape.groups.get(index);
            return (
              <Row
                key={index}
                line={shown}
                marker={shape.markers[index] ?? null}
                times={times}
                clock={clock}
                href={`?${withLine(params, step.id, shown.number).toString()}`}
                current={index === target}
                match={matches.has(shown.number)}
                annotation={annotated.get(index) ?? null}
                group={
                  group === undefined
                    ? null
                    : {
                        open: isOpen(toggles, group),
                        onToggle: () => setToggles((previous) => toggled(previous, group)),
                      }
                }
              />
            );
          })}
        </div>
      </div>
    </section>
  );
}

function LogSearch({ search, line, onGo }: SearchProps) {
  const { t } = useI18n();
  const matches = search.data ?? [];
  const at = matches.findIndex((match) => match.number === line);

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const text = String(new FormData(event.currentTarget).get('search') ?? '');
    if (text === '') {
      search.reset();
    } else {
      search.mutate(text);
    }
  };

  const move = (by: number) => {
    const from = at === -1 && by < 0 ? 0 : at;
    const next = matches[(from + by + matches.length) % matches.length];
    if (next !== undefined) {
      onGo(next.number);
    }
  };

  return (
    <Flex align="center" gap="2" wrap="wrap">
      <form onSubmit={submit} className="log-search">
        <Flex align="center" gap="2">
          <TextField.Root
            name="search"
            size="2"
            aria-label={t('runPage.log.search')}
            placeholder={t('runPage.log.search')}
            autoComplete="off"
            spellCheck={false}
          >
            <TextField.Slot>
              <Search size={14} aria-hidden />
            </TextField.Slot>
          </TextField.Root>
          <Button type="submit" variant="soft" loading={search.isPending}>
            {t('runPage.log.searchSubmit')}
          </Button>
        </Flex>
      </form>
      {search.isError ? <Failure error={search.error} /> : null}
      {search.isSuccess ? (
        <Flex align="center" gap="2" role="status">
          <Text size="2">
            {matches.length === 0
              ? t('runPage.log.noMatches')
              : t('runPage.log.matches', { current: at + 1, count: matches.length })}
          </Text>
          {matches.length === 0 ? null : (
            <>
              <IconButton
                variant="ghost"
                color="gray"
                aria-label={t('runPage.log.previousMatch')}
                onClick={() => move(-1)}
              >
                <ChevronUp size={16} />
              </IconButton>
              <IconButton
                variant="ghost"
                color="gray"
                aria-label={t('runPage.log.nextMatch')}
                onClick={() => move(1)}
              >
                <ChevronDown size={16} />
              </IconButton>
            </>
          )}
        </Flex>
      ) : null}
    </Flex>
  );
}

export function LogViewer({ job, step, times, onTimes, line, annotation }: LogViewerProps) {
  const { t } = useI18n();
  const api = useApi();
  const [, setParams] = useSearchParams();
  const log = useStepLog(step.execution_id, step.id, step.status === 'completed');
  const go = (number: number) =>
    setParams((previous) => withLine(previous, step.id, number), { replace: true });
  const search = useMutation({
    mutationFn: (text: string) => api.searchLog(step.execution_id, step.id, text),
    onSuccess: (found: LogLineData[]) => {
      const first = found[0];
      if (first !== undefined) {
        go(first.number);
      }
    },
  });
  const matches = useMemo(() => new Set((search.data ?? []).map((found) => found.number)), [search.data]);

  return (
    <Flex direction="column" gap="2">
      <Flex align="center" justify="between" gap="3" wrap="wrap">
        <LogSearch search={search} line={line} onGo={go} />
        <Flex align="center" gap="4" wrap="wrap">
          <Text as="label" size="2">
            <Flex as="span" align="center" gap="2">
              <Checkbox checked={times} onCheckedChange={(checked) => onTimes(checked === true)} />
              {t('runPage.log.times')}
            </Flex>
          </Text>
          <Button asChild variant="ghost" size="2">
            <a
              href={`/api/v1/executions/${step.execution_id}/steps/${step.id}/log`}
              download={`${job.key}-${step.index + 1}.log`}
            >
              <Download size={14} aria-hidden />
              {t('runPage.log.download')}
            </a>
          </Button>
        </Flex>
      </Flex>
      {log.state === 'loading' ? <Loading /> : null}
      {log.error === null ? null : <Failure error={log.error} />}
      {log.lines.length > 0 ? (
        <Lines
          step={step}
          lines={log.lines}
          times={times}
          following={log.state === 'following'}
          line={line}
          annotation={annotation}
          matches={matches}
        />
      ) : null}
      {log.lines.length === 0 && log.state === 'complete' ? (
        <Text as="p" size="2" color="gray">
          {t('runPage.log.empty')}
        </Text>
      ) : null}
      {log.state === 'following' ? (
        <Flex align="center" gap="2" role="status">
          <Spinner size="1" />
          <Text size="2" color="gray">
            {t('runPage.log.following')}
          </Text>
        </Flex>
      ) : null}
    </Flex>
  );
}
