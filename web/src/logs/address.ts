const STEP = 'step';

const LINE = 'line';

const ANNOTATION = 'annotation';

const WHOLE = /^\d+$/;

export interface LogAddress {
  step: string | null;
  line: number | null;
  annotation: number | null;
}

function whole(value: string | null, least: number): number | null {
  if (value === null || !WHOLE.test(value)) {
    return null;
  }
  const number = Number(value);
  return Number.isSafeInteger(number) && number >= least ? number : null;
}

export function addressOf(params: URLSearchParams): LogAddress {
  return {
    step: params.get(STEP),
    line: whole(params.get(LINE), 1),
    annotation: whole(params.get(ANNOTATION), 0),
  };
}

export function withStep(params: URLSearchParams, step: string | null): URLSearchParams {
  const next = new URLSearchParams(params);
  next.delete(LINE);
  next.delete(ANNOTATION);
  if (step === null) {
    next.delete(STEP);
  } else {
    next.set(STEP, step);
  }
  return next;
}

export function withLine(params: URLSearchParams, step: string, line: number): URLSearchParams {
  const next = withStep(params, step);
  next.set(LINE, String(line));
  return next;
}

export function withAnnotation(params: URLSearchParams, step: string, annotation: number): URLSearchParams {
  const next = withStep(params, step);
  next.set(ANNOTATION, String(annotation));
  return next;
}
