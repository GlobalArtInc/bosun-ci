import { describe, expect, it } from 'vitest';
import type { Job } from '../api/client';
import { BUILD, DEPLOY, LINT, LINUX_20, LINUX_22, MACOS_20, matrixJobs, TEST } from '../test/fixtures/graph';
import { GROUP_HEADER, type Layout, membersOf, type Placed, place, shapeOf, shownState } from './layout';

const NEEDS = [
  [BUILD, TEST],
  [BUILD, LINT],
  [TEST, DEPLOY],
  [LINT, DEPLOY],
];

function box(layout: Layout, id: string): Placed {
  const found = layout.nodes.find((node) => node.id === id);
  if (found === undefined) {
    throw new Error(`the layout has no node ${id}`);
  }
  return found;
}

function apart(left: Placed, right: Placed): boolean {
  return (
    left.x + left.width <= right.x ||
    right.x + right.width <= left.x ||
    left.y + left.height <= right.y ||
    right.y + right.height <= left.y
  );
}

function job(id: string): Job {
  const found = matrixJobs.find((known) => known.id === id);
  if (found === undefined) {
    throw new Error(`the fixture has no job ${id}`);
  }
  return found;
}

describe('the layout of the job graph', () => {
  it('draws a matrix group as one node and an edge per dependency between the jobs that are not members', async () => {
    const shape = shapeOf(matrixJobs, new Set());

    expect(shape.nodes).toEqual([
      { id: BUILD, kind: 'job', parent: null },
      { id: TEST, kind: 'group', parent: null },
      { id: LINT, kind: 'job', parent: null },
      { id: DEPLOY, kind: 'job', parent: null },
    ]);
    expect(shape.edges.map((edge) => [edge.source, edge.target])).toEqual(NEEDS);

    const layout = await place(shape);
    const build = box(layout, BUILD);
    const test = box(layout, TEST);
    const lint = box(layout, LINT);
    const deploy = box(layout, DEPLOY);
    expect(build.x + build.width).toBeLessThan(test.x);
    expect(test.x).toBe(lint.x);
    expect(test.x + test.width).toBeLessThan(deploy.x);
    expect(apart(test, lint)).toBe(true);
    expect(layout.width).toBeGreaterThanOrEqual(deploy.x + deploy.width);
  });

  it('places the members of an expanded group inside it, below its header, in their order', async () => {
    const shape = shapeOf(matrixJobs, new Set([TEST]));

    expect(shape.nodes.filter((node) => node.parent === TEST)).toEqual([
      { id: LINUX_20, kind: 'member', parent: TEST },
      { id: LINUX_22, kind: 'member', parent: TEST },
      { id: MACOS_20, kind: 'member', parent: TEST },
    ]);
    expect(shape.edges.map((edge) => [edge.source, edge.target])).toEqual(NEEDS);

    const layout = await place(shape);
    const group = box(layout, TEST);
    expect(group.kind).toBe('expanded');
    const members = [LINUX_20, LINUX_22, MACOS_20].map((id) => box(layout, id));
    for (const member of members) {
      expect(member.parent).toBe(TEST);
      expect(member.x).toBeGreaterThanOrEqual(0);
      expect(member.y).toBeGreaterThanOrEqual(GROUP_HEADER);
      expect(member.x + member.width).toBeLessThanOrEqual(group.width);
      expect(member.y + member.height).toBeLessThanOrEqual(group.height);
    }
    expect(members.map((member) => member.y)).toEqual(
      [...members.map((member) => member.y)].sort((a, b) => a - b),
    );
    expect(apart(group, box(layout, LINT))).toBe(true);
  });

  it('shows a group by its most active member until the group completes', () => {
    const members = membersOf(matrixJobs).get(TEST) ?? [];
    const group = job(TEST);

    expect(shownState(group, members).status).toBe('in_progress');
    expect(shownState(group, members.slice(2))).toEqual({
      status: 'waiting',
      conclusion: null,
      conclusion_source: null,
      waiting_reason: 'max_parallel',
    });
    const completed: Job = {
      ...group,
      status: 'completed',
      conclusion: 'failure',
      conclusion_source: 'natural',
    };
    expect(shownState(completed, members)).toBe(completed);
    expect(shownState(job(BUILD), [])).toBe(job(BUILD));
  });
});
