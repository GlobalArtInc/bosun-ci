import { useEffect, useState } from 'react';
import type { I18n } from '../i18n/i18n';

const SECOND_MS = 1000;

const UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ['day', 86_400],
  ['hour', 3_600],
  ['minute', 60],
  ['second', 1],
];

export function useNow(ticking: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!ticking) {
      return;
    }
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), SECOND_MS);
    return () => clearInterval(timer);
  }, [ticking]);
  return now;
}

export function span(start: string | null, end: string | null, now: number): number | null {
  if (start === null) {
    return null;
  }
  return Math.max(0, (end === null ? now : Date.parse(end)) - Date.parse(start));
}

export function duration(t: I18n['t'], milliseconds: number): string {
  const total = Math.round(milliseconds / SECOND_MS);
  const hours = Math.floor(total / 3_600);
  const minutes = Math.floor((total % 3_600) / 60);
  const seconds = total % 60;
  if (hours > 0) {
    return t('time.hours', { hours, minutes });
  }
  if (minutes > 0) {
    return t('time.minutes', { minutes, seconds });
  }
  return t('time.seconds', { seconds });
}

export function ago(language: string, at: string, now: number): string {
  const seconds = Math.round((Date.parse(at) - now) / SECOND_MS);
  const format = new Intl.RelativeTimeFormat(language, { numeric: 'auto' });
  for (const [unit, size] of UNITS) {
    if (Math.abs(seconds) >= size || unit === 'second') {
      return format.format(Math.round(seconds / size), unit);
    }
  }
  return format.format(seconds, 'second');
}

export function stamp(language: string, at: string): string {
  return new Intl.DateTimeFormat(language, { dateStyle: 'medium', timeStyle: 'medium' }).format(
    Date.parse(at),
  );
}
