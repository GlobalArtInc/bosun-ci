import type { ElkNode, LayoutOptions } from 'elkjs/lib/elk-api';
import type { Job, JobStatus } from '../api/client';

export type Kind = 'job' | 'group' | 'expanded' | 'member';

interface Shaped {
  id: string;
  kind: Kind;
  parent: string | null;
}

interface Dependency {
  id: string;
  source: string;
  target: string;
}

interface Shape {
  nodes: Shaped[];
  edges: Dependency[];
}

export interface Placed extends Shaped {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface Layout {
  nodes: Placed[];
  edges: Dependency[];
  width: number;
  height: number;
}

interface Shown {
  status: JobStatus;
  conclusion: Job['conclusion'];
  conclusion_source: Job['conclusion_source'];
  waiting_reason: Job['waiting_reason'];
}

const NODE_WIDTH = 280;

const NODE_HEIGHT = 100;

export const GROUP_HEADER = 64;

const GROUP_PADDING = 12;

const ROOT: LayoutOptions = {
  'elk.algorithm': 'layered',
  'elk.direction': 'RIGHT',
  'elk.spacing.nodeNode': '24',
  'elk.layered.spacing.nodeNodeBetweenLayers': '72',
  'elk.layered.considerModelOrder.strategy': 'NODES_AND_EDGES',
};

const GROUP: LayoutOptions = {
  'elk.algorithm': 'layered',
  'elk.direction': 'RIGHT',
  'elk.spacing.nodeNode': String(GROUP_PADDING),
  'elk.layered.considerModelOrder.strategy': 'NODES_AND_EDGES',
  'elk.padding': `[top=${GROUP_HEADER},left=${GROUP_PADDING},bottom=${GROUP_PADDING},right=${GROUP_PADDING}]`,
};

const ACTIVITY: JobStatus[] = ['held', 'paused', 'in_progress', 'assigned', 'queued', 'waiting', 'pending'];

export function membersOf(jobs: Job[]): Map<string, Job[]> {
  const members = new Map<string, Job[]>();
  for (const job of jobs) {
    if (job.group_id !== null) {
      members.set(job.group_id, [...(members.get(job.group_id) ?? []), job]);
    }
  }
  return members;
}

export function shapeOf(jobs: Job[], expanded: ReadonlySet<string>): Shape {
  const tops = jobs.filter((job) => job.group_id === null);
  const members = membersOf(jobs);
  const nodes: Shaped[] = [];
  for (const top of tops) {
    const own = members.get(top.id) ?? [];
    if (own.length === 0) {
      nodes.push({ id: top.id, kind: 'job', parent: null });
    } else if (!expanded.has(top.id)) {
      nodes.push({ id: top.id, kind: 'group', parent: null });
    } else {
      nodes.push({ id: top.id, kind: 'expanded', parent: null });
      for (const member of own) {
        nodes.push({ id: member.id, kind: 'member', parent: top.id });
      }
    }
  }
  const edges: Dependency[] = [];
  for (const top of tops) {
    for (const need of new Set(top.needs)) {
      const source = tops.find((job) => job.key === need);
      if (source !== undefined && source.id !== top.id) {
        edges.push({ id: `${source.id}>${top.id}`, source: source.id, target: top.id });
      }
    }
  }
  return { nodes, edges };
}

export function shownState(job: Job, members: Job[]): Shown {
  if (job.status === 'completed' || members.length === 0) {
    return job;
  }
  const status = ACTIVITY.find((candidate) => members.some((member) => member.status === candidate));
  const first = members.find((member) => member.status === status);
  if (status === undefined || first === undefined) {
    return job;
  }
  return { status, conclusion: null, conclusion_source: null, waiting_reason: first.waiting_reason };
}

function elkNode(node: Shaped, nodes: Shaped[]): ElkNode {
  if (node.kind !== 'expanded') {
    return { id: node.id, width: NODE_WIDTH, height: NODE_HEIGHT };
  }
  return {
    id: node.id,
    layoutOptions: GROUP,
    children: nodes
      .filter((member) => member.parent === node.id)
      .map((member) => ({ id: member.id, width: NODE_WIDTH, height: NODE_HEIGHT })),
  };
}

function placed(node: ElkNode, shaped: Map<string, Shaped>): Placed[] {
  const own = shaped.get(node.id);
  if (own === undefined) {
    return [];
  }
  const box: Placed = {
    ...own,
    x: node.x ?? 0,
    y: node.y ?? 0,
    width: node.width ?? NODE_WIDTH,
    height: node.height ?? NODE_HEIGHT,
  };
  return [box, ...(node.children ?? []).flatMap((child) => placed(child, shaped))];
}

export async function place(shape: Shape): Promise<Layout> {
  const { default: Elk } = await import('elkjs/lib/elk.bundled.js');
  const graph: ElkNode = {
    id: 'run',
    layoutOptions: ROOT,
    children: shape.nodes.filter((node) => node.parent === null).map((node) => elkNode(node, shape.nodes)),
    edges: shape.edges.map((edge) => ({ id: edge.id, sources: [edge.source], targets: [edge.target] })),
  };
  const laid: ElkNode = await new Elk().layout(graph);
  const shaped = new Map(shape.nodes.map((node) => [node.id, node]));
  return {
    nodes: (laid.children ?? []).flatMap((child) => placed(child, shaped)),
    edges: shape.edges,
    width: laid.width ?? 0,
    height: laid.height ?? 0,
  };
}
