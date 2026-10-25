import { describe, expect, it } from 'vitest';
import type { Annotation } from '../api/client';
import { type LogLine, lineOf, linesOf, placeAnnotations, rowsOf, structure } from './lines';

const TIME = '2026-10-24T18:34:51.123456789Z';

function log(texts: string[]): LogLine[] {
  return texts.map((text, index) => ({ number: index + 1, time: TIME, text }));
}

function annotation(kind: Annotation['kind'], message: string): Annotation {
  return {
    kind,
    message,
    title: null,
    file: null,
    line: null,
    end_line: null,
    column: null,
    end_column: null,
  };
}

describe('the lines of a step log', () => {
  it('split each line into its time and its text, numbered from 1', () => {
    expect(linesOf(`${TIME} first\n${TIME} \n${TIME} third with  spaces\n`)).toEqual([
      { number: 1, time: TIME, text: 'first' },
      { number: 2, time: TIME, text: '' },
      { number: 3, time: TIME, text: 'third with  spaces' },
    ]);
    expect(lineOf(12, TIME)).toEqual({ number: 12, time: TIME, text: '' });
  });

  it('fold an ended group, keep an open one unfolded, and never show the end of a group', () => {
    const lines = log([
      'a',
      '##[group]one',
      'b',
      '##[endgroup]',
      'c',
      '##[group]two',
      'd',
      '##[group]three',
      'e',
    ]);
    const shape = structure(lines);

    expect(rowsOf(shape, (group) => !group.ended)).toEqual([0, 1, 4, 5, 7, 8]);
    expect(rowsOf(shape, () => true)).toEqual([0, 1, 2, 4, 5, 6, 7, 8]);
    expect(shape.groups.get(5)).toEqual({ header: 5, end: 7, ended: true });
    expect(shape.groups.get(7)).toEqual({ header: 7, end: 9, ended: false });
    expect(shape.groupOf).toEqual([-1, -1, 1, 1, -1, -1, 5, -1, 7]);
  });

  it('place each annotation at the next line of its kind that holds the first line of its message', () => {
    const lines = log([
      '##[error]a line of output that looks like an error',
      '##[warning]unused import',
      '##[error]first',
      'second',
      '##[error]src/app.ts(3,5): \u001b[31merror\u001b[0m TS2322: the type is wrong',
      '##[warning]unused import',
      '##[warning]dropped by the agent',
    ]);
    const annotations = [
      annotation('error', 'first\nsecond'),
      annotation('warning', 'unused import'),
      annotation('error', 'TS2322: the type is wrong'),
      annotation('warning', 'unused import'),
      annotation('notice', 'nowhere in the log'),
    ];

    expect(placeAnnotations(lines, structure(lines), annotations)).toEqual([2, 1, 4, 5, null]);
  });
});
