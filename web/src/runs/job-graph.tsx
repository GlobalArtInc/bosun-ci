import { Flex, IconButton, Link, Text } from '@radix-ui/themes';
import { keepPreviousData, useQuery } from '@tanstack/react-query';
import {
  Controls,
  type Edge,
  getViewportForBounds,
  Handle,
  type Node,
  type NodeHandle,
  type NodeProps,
  Position,
  ReactFlow,
  useReactFlow,
  useStore,
} from '@xyflow/react';
import '@xyflow/react/dist/base.css';
import { ChevronDown, ChevronRight } from 'lucide-react';
import { useCallback, useEffect, useMemo, useState } from 'react';
import { Link as RouterLink } from 'react-router';
import type { Job } from '../api/client';
import { useI18n } from '../i18n/i18n';
import { Failure, Loading } from '../shell/status';
import { useTheme } from '../theme/theme';
import { type Kind, type Layout, membersOf, type Placed, place, shapeOf, shownState } from './layout';
import { StateBadge, toneOf, WAITING } from './state';
import { duration, span, useNow } from './time';

interface JobGraphProps {
  jobs: Job[];
  selected: string | null;
}

interface HeaderProps {
  card: Card;
}

interface RefitProps {
  signature: string;
  width: number;
  height: number;
}

type Card = {
  job: Job;
  kind: Kind;
  members: Job[];
  current: boolean;
  onToggle: (id: string) => void;
};

type CardNode = Node<Card, 'card'>;

const LAYOUT = 'layout';

const MARGIN = 24;

const LOWEST = 160;

const HIGHEST = 560;

const FIT = { padding: 0.08, maxZoom: 1 };

const LEAST_ZOOM = 0.2;

const UNTOLD = { 'aria-roledescription': undefined };

const POINTED = { pointerEvents: 'auto' } as const;

function valueText(value: unknown): string {
  return typeof value === 'string' ? value : JSON.stringify(value);
}

function matrixText(matrix: Job['matrix']): string {
  if (matrix === null) {
    return '';
  }
  return Object.entries(matrix)
    .map(([name, value]) => `${name}: ${valueText(value)}`)
    .join(', ');
}

function handlesOf(box: Placed): NodeHandle[] {
  const middle = box.height / 2;
  return [
    { type: 'target', position: Position.Left, x: 0, y: middle },
    { type: 'source', position: Position.Right, x: box.width - 1, y: middle },
  ];
}

function Toggle({ card }: HeaderProps) {
  const { t } = useI18n();
  const open = card.kind === 'expanded';
  return (
    <IconButton
      size="1"
      variant="ghost"
      color="gray"
      className="nopan"
      aria-expanded={open}
      aria-label={
        open
          ? t('runPage.graph.collapse', { name: card.job.name })
          : t('runPage.graph.expand', { name: card.job.name })
      }
      onClick={() => card.onToggle(card.job.id)}
    >
      {open ? <ChevronDown size={14} aria-hidden /> : <ChevronRight size={14} aria-hidden />}
    </IconButton>
  );
}

function Header({ card }: HeaderProps) {
  const { t } = useI18n();
  const { job, kind, members } = card;
  const shown = shownState(job, members);
  const grouped = kind === 'group' || kind === 'expanded';
  const now = useNow(!grouped && job.started_at !== null && job.completed_at === null);
  const took = grouped ? null : span(job.started_at, job.completed_at, now);
  const done = members.filter((member) => member.status === 'completed').length;
  const detail = grouped
    ? t('runPage.graph.complete', { done, count: members.length })
    : matrixText(job.matrix);
  return (
    <>
      <Flex align="center" justify="between" gap="2">
        <Flex align="center" gap="1" minWidth="0">
          {grouped ? <Toggle card={card} /> : null}
          <Link asChild size="2" weight="medium" truncate>
            <RouterLink
              to={`?job=${job.id}`}
              className="job-link nopan"
              title={job.name}
              aria-current={card.current ? 'true' : undefined}
            >
              {job.name}
            </RouterLink>
          </Link>
        </Flex>
        <StateBadge
          size="1"
          status={shown.status}
          conclusion={shown.conclusion}
          source={shown.conclusion_source}
        />
      </Flex>
      <Flex align="center" justify="between" gap="2" minHeight="18px">
        <Text size="1" color="gray" truncate title={detail}>
          {detail}
        </Text>
        {took === null ? null : (
          <Text size="1" className="job-time">
            {duration(t, took)}
          </Text>
        )}
      </Flex>
    </>
  );
}

function Reason({ card }: HeaderProps) {
  const { t } = useI18n();
  const shown = shownState(card.job, card.members);
  if (shown.status !== 'waiting' || shown.waiting_reason === null) {
    return null;
  }
  const reason = t(WAITING[shown.waiting_reason]);
  return (
    <Text size="1" color="amber" highContrast className="job-reason" title={reason}>
      {reason}
    </Text>
  );
}

function JobCard({ data }: NodeProps<CardNode>) {
  const shown = shownState(data.job, data.members);
  return (
    <div
      className={data.kind === 'expanded' ? 'job-node job-group' : 'job-node'}
      data-tone={toneOf(shown.status, shown.conclusion)}
      data-current={data.current ? 'true' : undefined}
    >
      <Handle type="target" position={Position.Left} isConnectable={false} className="job-handle" />
      <Header card={data} />
      {data.kind === 'expanded' ? null : <Reason card={data} />}
      <Handle type="source" position={Position.Right} isConnectable={false} className="job-handle" />
    </div>
  );
}

const NODE_TYPES = { card: JobCard };

function Refit({ signature, width, height }: RefitProps) {
  const flow = useReactFlow();
  const boxWidth = useStore((state) => state.width);
  const boxHeight = useStore((state) => state.height);
  useEffect(() => {
    if (signature !== '' && width > 0 && height > 0 && boxWidth > 0 && boxHeight > 0) {
      const bounds = { x: 0, y: 0, width, height };
      void flow.setViewport(
        getViewportForBounds(bounds, boxWidth, boxHeight, LEAST_ZOOM, FIT.maxZoom, FIT.padding),
      );
    }
  }, [flow, signature, width, height, boxWidth, boxHeight]);
  return null;
}

function signatureOf(layout: Layout): string {
  return layout.nodes.map((node) => `${node.id}:${node.parent ?? ''}`).join(',');
}

export function JobGraph({ jobs, selected }: JobGraphProps) {
  const { t } = useI18n();
  const { appearance } = useTheme();
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  const shape = useMemo(() => shapeOf(jobs, expanded), [jobs, expanded]);
  const layout = useQuery({
    queryKey: [LAYOUT, shape],
    queryFn: () => place(shape),
    staleTime: Number.POSITIVE_INFINITY,
    placeholderData: keepPreviousData,
  });
  const labels = useMemo(
    () => ({
      'node.a11yDescription.default': '',
      'node.a11yDescription.keyboardDisabled': '',
      'edge.a11yDescription.default': '',
      'controls.ariaLabel': t('runPage.graph.controls'),
      'controls.zoomIn.ariaLabel': t('runPage.graph.zoomIn'),
      'controls.zoomOut.ariaLabel': t('runPage.graph.zoomOut'),
      'controls.fitView.ariaLabel': t('runPage.graph.fit'),
    }),
    [t],
  );
  const onToggle = useCallback((id: string) => {
    setExpanded((open) => {
      const next = new Set(open);
      if (!next.delete(id)) {
        next.add(id);
      }
      return next;
    });
  }, []);

  if (layout.isError) {
    return <Failure error={layout.error} onRetry={() => layout.refetch()} />;
  }
  if (layout.data === undefined) {
    return <Loading />;
  }
  const byId = new Map(jobs.map((job) => [job.id, job]));
  const members = membersOf(jobs);
  const nodes: CardNode[] = layout.data.nodes.flatMap((box) => {
    const job = byId.get(box.id);
    if (job === undefined) {
      return [];
    }
    const own = box.kind === 'group' || box.kind === 'expanded' ? (members.get(job.id) ?? []) : [];
    const current =
      selected === job.id || (box.kind === 'group' && own.some((member) => member.id === selected));
    return [
      {
        id: job.id,
        type: 'card',
        position: { x: box.x, y: box.y },
        width: box.width,
        height: box.height,
        handles: handlesOf(box),
        parentId: box.parent ?? undefined,
        style: POINTED,
        draggable: false,
        selectable: false,
        ariaRole: 'group',
        ariaLabel: job.name,
        domAttributes: UNTOLD,
        data: { job, kind: box.kind, members: own, current, onToggle },
      },
    ];
  });
  const edges: Edge[] = layout.data.edges.flatMap((edge) => {
    const source = byId.get(edge.source);
    const target = byId.get(edge.target);
    if (source === undefined || target === undefined) {
      return [];
    }
    return [
      {
        id: edge.id,
        source: edge.source,
        target: edge.target,
        selectable: false,
        focusable: false,
        ariaLabel: t('runPage.graph.needs', { job: target.name, need: source.name }),
        domAttributes: UNTOLD,
      },
    ];
  });
  const height = Math.min(HIGHEST, Math.max(LOWEST, layout.data.height + 2 * MARGIN));
  return (
    <div className="job-graph" style={{ height }}>
      <ReactFlow
        aria-label={t('runPage.jobs.title')}
        nodes={nodes}
        edges={edges}
        nodeTypes={NODE_TYPES}
        fitView
        fitViewOptions={FIT}
        minZoom={LEAST_ZOOM}
        maxZoom={1.5}
        nodesDraggable={false}
        nodesConnectable={false}
        nodesFocusable={false}
        edgesFocusable={false}
        elementsSelectable={false}
        disableKeyboardA11y
        zoomOnScroll={false}
        zoomOnDoubleClick={false}
        preventScrolling={false}
        ariaLabelConfig={labels}
        colorMode={appearance}
      >
        <Controls showInteractive={false} fitViewOptions={FIT} />
        <Refit signature={signatureOf(layout.data)} width={layout.data.width} height={layout.data.height} />
      </ReactFlow>
    </div>
  );
}
