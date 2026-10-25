import type { Severity } from '../api/client';
import type { Key } from '../i18n/i18n';

interface SeverityLook {
  key: Key;
  color: 'red' | 'amber' | 'blue';
}

const SEVERITY_ORDER: Record<Severity, number> = { error: 0, warning: 1, notice: 2 };

export const SEVERITIES: Record<Severity, SeverityLook> = {
  error: { key: 'runPage.notices.error', color: 'red' },
  warning: { key: 'runPage.notices.warning', color: 'amber' },
  notice: { key: 'runPage.notices.notice', color: 'blue' },
};

export function bySeverity<T>(items: T[], severity: (item: T) => Severity): T[] {
  return items
    .map((item, order) => ({ item, order }))
    .sort(
      (left, right) =>
        SEVERITY_ORDER[severity(left.item)] - SEVERITY_ORDER[severity(right.item)] ||
        left.order - right.order,
    )
    .map(({ item }) => item);
}
