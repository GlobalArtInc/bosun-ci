import type { Step } from '../api/client';

interface StepNode {
  step: Step;
  pre: Step[];
  post: Step[];
}

const PRE = 'Pre ';

const POST = 'Post ';

function attached(name: string): boolean {
  return name.startsWith(PRE) || name.startsWith(POST);
}

function parentOf(step: Step, ordered: Step[]): Step | undefined {
  if (step.name.startsWith(PRE)) {
    const main = step.name.slice(PRE.length);
    return ordered.find((other) => other.index > step.index && other.name === main && !attached(other.name));
  }
  if (step.name.startsWith(POST)) {
    const main = step.name.slice(POST.length);
    return ordered.findLast(
      (other) => other.index < step.index && other.name === main && !attached(other.name),
    );
  }
  return undefined;
}

export function nest(steps: Step[]): StepNode[] {
  const ordered = [...steps].sort((left, right) => left.index - right.index);
  const parents = new Map(ordered.map((step) => [step.id, parentOf(step, ordered)]));
  const nodes = new Map<string, StepNode>();
  for (const step of ordered) {
    if (parents.get(step.id) === undefined) {
      nodes.set(step.id, { step, pre: [], post: [] });
    }
  }
  for (const step of ordered) {
    const parent = parents.get(step.id);
    const node = parent === undefined ? undefined : nodes.get(parent.id);
    if (node !== undefined) {
      (step.name.startsWith(PRE) ? node.pre : node.post).push(step);
    }
  }
  return [...nodes.values()];
}
