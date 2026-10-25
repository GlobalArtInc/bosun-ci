import type { Annotation, AnnotationKind } from '../api/client';
import { plain } from './ansi';

export interface LogLine {
  number: number;
  time: string;
  text: string;
}

export type Marker = AnnotationKind | 'debug' | 'group' | 'endgroup';

interface Marked {
  marker: Marker | null;
  rest: string;
}

export interface Group {
  header: number;
  end: number;
  ended: boolean;
}

export interface Structure {
  markers: (Marker | null)[];
  groups: Map<number, Group>;
  groupOf: number[];
}

const MARKERS = new Set<string>(['group', 'endgroup', 'error', 'warning', 'notice', 'debug']);

const OPENING = '##[';

const NONE = -1;

export function marked(text: string): Marked {
  if (!text.startsWith(OPENING)) {
    return { marker: null, rest: text };
  }
  const close = text.indexOf(']', OPENING.length);
  const name = close === -1 ? '' : text.slice(OPENING.length, close);
  if (!MARKERS.has(name)) {
    return { marker: null, rest: text };
  }
  return { marker: name as Marker, rest: text.slice(close + 1) };
}

export function lineOf(number: number, line: string): LogLine {
  const space = line.indexOf(' ');
  if (space === -1) {
    return { number, time: line, text: '' };
  }
  return { number, time: line.slice(0, space), text: line.slice(space + 1) };
}

export function linesOf(body: string): LogLine[] {
  const raw = body.split('\n');
  if (raw[raw.length - 1] === '') {
    raw.pop();
  }
  return raw.map((line, index) => lineOf(index + 1, line));
}

export function structure(lines: LogLine[]): Structure {
  const markers = lines.map((line) => marked(line.text).marker);
  const groups = new Map<number, Group>();
  const groupOf = new Array<number>(lines.length).fill(NONE);
  let open: Group | null = null;
  markers.forEach((marker, index) => {
    if (marker === 'group') {
      if (open !== null) {
        open.end = index;
        open.ended = true;
      }
      open = { header: index, end: lines.length, ended: false };
      groups.set(index, open);
      return;
    }
    if (open !== null) {
      groupOf[index] = open.header;
    }
    if (marker === 'endgroup' && open !== null) {
      open.end = index;
      open.ended = true;
      open = null;
    }
  });
  return { markers, groups, groupOf };
}

export function rowsOf(shape: Structure, isOpen: (group: Group) => boolean): number[] {
  const rows: number[] = [];
  for (let index = 0; index < shape.markers.length; index += 1) {
    const marker = shape.markers[index];
    if (marker === 'endgroup') {
      continue;
    }
    rows.push(index);
    const group = marker === 'group' ? shape.groups.get(index) : undefined;
    if (group !== undefined && !isOpen(group)) {
      index = group.end - 1;
    }
  }
  return rows;
}

export function placeAnnotations(
  lines: LogLine[],
  shape: Structure,
  annotations: Annotation[],
): (number | null)[] {
  const next: Record<AnnotationKind, number> = { error: 0, warning: 0, notice: 0 };
  return annotations.map((annotation) => {
    const wanted = plain(annotation.message.split('\n')[0] ?? '');
    for (let index = next[annotation.kind]; index < lines.length; index += 1) {
      const line = lines[index];
      if (
        line !== undefined &&
        shape.markers[index] === annotation.kind &&
        plain(marked(line.text).rest).includes(wanted)
      ) {
        next[annotation.kind] = index + 1;
        return index;
      }
    }
    return null;
  });
}

export function indexOfLine(lines: LogLine[], number: number): number | null {
  const guess = lines[number - 1];
  if (guess?.number === number) {
    return number - 1;
  }
  const found = lines.findIndex((line) => line.number === number);
  return found === -1 ? null : found;
}

export function rowAtOrBefore(rows: number[], index: number): number {
  let low = 0;
  let high = rows.length - 1;
  let found = 0;
  while (low <= high) {
    const middle = (low + high) >> 1;
    if ((rows[middle] ?? 0) <= index) {
      found = middle;
      low = middle + 1;
    } else {
      high = middle - 1;
    }
  }
  return found;
}

export function location(annotation: Annotation): string | null {
  if (annotation.file === null) {
    return null;
  }
  if (annotation.line === null) {
    return annotation.file;
  }
  return annotation.column === null
    ? `${annotation.file}:${annotation.line}`
    : `${annotation.file}:${annotation.line}:${annotation.column}`;
}
