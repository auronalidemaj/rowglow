import { graphlib, layout } from '@dagrejs/dagre';
import clsx from 'clsx';
import { Copy, Download, KeyRound, Link2, Maximize2, Minus, Plus, RefreshCw, Search } from 'lucide-react';
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { copyText, pickSaveFile } from '../lib/platform';
import { call, errorMessage } from '../lib/rpc';
import type { CompletionTable, SchemaRelations } from '../lib/types';
import { useStore, type DiagramTab, type Workspace } from '../state/store';
import { Button, EmptyState, IconButton, Segmented, Spinner } from './ui';

const HEAD_H = 32;
const ROW_H = 21;
const PAD_B = 6;
const CHAR_W = 6.6;
const MIN_W = 170;
const MAX_W = 340;

interface Col {
  name: string;
  type: string;
  pk: boolean;
  fk: boolean;
}

interface TableNode {
  name: string;
  kind: string;
  cols: Col[];
  x: number;
  y: number;
  w: number;
  h: number;
}

interface Edge {
  name: string;
  from: string; // child table (has the FK)
  fromCol: string;
  to: string; // referenced table
  toCol: string;
}

interface Palette {
  bg: string;
  card: string;
  head: string;
  border: string;
  text: string;
  dim: string;
  faint: string;
  accent: string;
  edge: string;
}

function readPalette(): Palette {
  const cs = getComputedStyle(document.documentElement);
  const v = (name: string, fallback: string) => cs.getPropertyValue(name).trim() || fallback;
  const dark = document.documentElement.dataset.theme === 'dark';
  return {
    bg: v('--bg', dark ? '#16171a' : '#ffffff'),
    card: v('--bg-elev', dark ? '#1e1f23' : '#ffffff'),
    head: v('--bg-sunken', dark ? '#25262b' : '#f4f4f5'),
    border: v('--border-strong', dark ? '#3a3b41' : '#d4d4d8'),
    text: v('--text', dark ? '#ececee' : '#18181b'),
    dim: v('--text-dim', dark ? '#a1a1aa' : '#52525b'),
    faint: v('--text-faint', dark ? '#71717a' : '#a1a1aa'),
    accent: v('--accent', '#F2A93B'),
    edge: dark ? '#6b7280' : '#9ca3af',
  };
}

function build(tables: CompletionTable[], rel: SchemaRelations, keysOnly: boolean) {
  const pks = new Map(rel.primaryKeys.map((p) => [p.table, new Set(p.columns)]));
  const edges: Edge[] = [];
  const fkCols = new Map<string, Set<string>>();
  const names = new Set(tables.map((t) => t.name));
  for (const fk of rel.foreignKeys) {
    if (!names.has(fk.table) || !names.has(fk.refTable)) continue;
    edges.push({ name: fk.name, from: fk.table, fromCol: fk.columns[0] ?? '', to: fk.refTable, toCol: fk.refColumns[0] ?? '' });
    const set = fkCols.get(fk.table) ?? new Set<string>();
    fk.columns.forEach((c) => set.add(c));
    fkCols.set(fk.table, set);
  }
  const nodes = new Map<string, TableNode>();
  for (const t of tables) {
    const pk = pks.get(t.name) ?? new Set<string>();
    const fks = fkCols.get(t.name) ?? new Set<string>();
    let cols: Col[] = t.columns.map(([name, type]) => ({ name, type: type.replace(/ without time zone| with time zone/g, ''), pk: pk.has(name), fk: fks.has(name) }));
    if (keysOnly) cols = cols.filter((c) => c.pk || c.fk);
    const longest = Math.max(t.name.length * 7.4 + 30, ...cols.map((c) => (c.name.length + c.type.length) * CHAR_W + 52));
    const w = Math.round(Math.min(MAX_W, Math.max(MIN_W, longest)));
    nodes.set(t.name, { name: t.name, kind: t.kind, cols, x: 0, y: 0, w, h: HEAD_H + Math.max(1, cols.length) * ROW_H + PAD_B });
  }

  // Lay out tables that have relationships with dagre (parents on the left),
  // then put unrelated tables in a grid underneath.
  const linked = new Set(edges.flatMap((e) => [e.from, e.to]));
  const g = new graphlib.Graph({ multigraph: true });
  g.setGraph({ rankdir: 'LR', nodesep: 28, ranksep: 90, marginx: 30, marginy: 30 });
  g.setDefaultEdgeLabel(() => ({}));
  for (const n of nodes.values()) if (linked.has(n.name)) g.setNode(n.name, { width: n.w, height: n.h });
  edges.forEach((e, i) => {
    if (e.from !== e.to) g.setEdge(e.to, e.from, {}, `${e.name}:${i}`);
  });
  let maxX = 0;
  let maxY = 0;
  if (linked.size) {
    layout(g);
    for (const id of g.nodes()) {
      const p = g.node(id);
      const n = nodes.get(id)!;
      n.x = p.x - n.w / 2;
      n.y = p.y - n.h / 2;
      maxX = Math.max(maxX, n.x + n.w);
      maxY = Math.max(maxY, n.y + n.h);
    }
  }
  const loose = [...nodes.values()].filter((n) => !linked.has(n.name)).sort((a, b) => a.name.localeCompare(b.name));
  const wrap = Math.max(maxX, 1100);
  let x = 30;
  let y = linked.size ? maxY + 60 : 30;
  let rowH = 0;
  for (const n of loose) {
    if (x + n.w > wrap && x > 30) {
      x = 30;
      y += rowH + 28;
      rowH = 0;
    }
    n.x = x;
    n.y = y;
    x += n.w + 28;
    rowH = Math.max(rowH, n.h);
  }
  const all = [...nodes.values()];
  const width = Math.max(...all.map((n) => n.x + n.w), 200) + 30;
  const height = Math.max(...all.map((n) => n.y + n.h), 200) + 30;
  return { nodes, edges, width, height };
}

function rowY(n: TableNode, col: string): number {
  const i = n.cols.findIndex((c) => c.name === col);
  return n.y + HEAD_H + (i < 0 ? 0 : i) * ROW_H + ROW_H / 2 - (i < 0 ? ROW_H / 2 - 2 : 0);
}

function edgePath(a: TableNode, aCol: string, b: TableNode, bCol: string): { d: string; ax: number; ay: number; bx: number; by: number; aDir: number; bDir: number } {
  const ay = rowY(a, aCol);
  const by = rowY(b, bCol);
  if (a === b) {
    const x = a.x + a.w;
    return { d: `M ${x} ${ay} C ${x + 46} ${ay}, ${x + 46} ${by}, ${x} ${by}`, ax: x, ay, bx: x, by, aDir: 1, bDir: 1 };
  }
  const aCenter = a.x + a.w / 2;
  const bCenter = b.x + b.w / 2;
  const right = bCenter >= aCenter;
  const ax = right ? a.x + a.w : a.x;
  const bx = right ? b.x : b.x + b.w;
  const dx = Math.max(36, Math.abs(bx - ax) / 2);
  const s = right ? 1 : -1;
  return { d: `M ${ax} ${ay} C ${ax + dx * s} ${ay}, ${bx - dx * s} ${by}, ${bx} ${by}`, ax, ay, bx, by, aDir: s, bDir: -s };
}

function mermaid(tables: TableNode[], edges: Edge[]): string {
  const id = (s: string) => (/^[A-Za-z_][A-Za-z0-9_]*$/.test(s) ? s : `"${s.replace(/"/g, '')}"`);
  const type = (s: string) => s.replace(/[^A-Za-z0-9_]/g, '_') || 'any';
  const lines = ['erDiagram'];
  for (const t of tables) {
    lines.push(`  ${id(t.name)} {`);
    for (const c of t.cols) lines.push(`    ${type(c.type)} ${id(c.name)}${c.pk ? ' PK' : c.fk ? ' FK' : ''}`);
    lines.push('  }');
  }
  for (const e of edges) lines.push(`  ${id(e.to)} ||--o{ ${id(e.from)} : "${e.fromCol}"`);
  return lines.join('\n') + '\n';
}

export function ErDiagram({ ws, tab, active }: { ws: Workspace; tab: DiagramTab; active: boolean }) {
  const { openTable, toast } = useStore.getState();
  const theme = useStore((s) => s.settings.theme);
  const [data, setData] = useState<{ tables: CompletionTable[]; relations: SchemaRelations } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [keysOnly, setKeysOnly] = useState<boolean | null>(null);
  const [view, setView] = useState({ x: 0, y: 0, k: 1 });
  const [hover, setHover] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [query, setQuery] = useState('');
  const host = useRef<HTMLDivElement>(null);
  const svgRef = useRef<SVGSVGElement>(null);
  const fitted = useRef(false);
  const palette = useMemo(readPalette, [theme, active]);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const r = await call<{ tables: CompletionTable[]; relations: SchemaRelations }>('meta.relations', { sessionId: ws.sessionId, schema: tab.schema });
      setData({ tables: r.tables.filter((t) => t.kind === 'table' || t.kind === 'view'), relations: r.relations });
      fitted.current = false;
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setLoading(false);
    }
  }, [ws.sessionId, tab.schema]);

  useEffect(() => {
    void load();
  }, [load, ws.refreshKey]);

  const compact = keysOnly ?? (data ? data.tables.length > 40 : false);
  const graph = useMemo(() => (data ? build(data.tables, data.relations, compact) : null), [data, compact]);

  const fit = useCallback(() => {
    const el = host.current;
    if (!el || !graph) return;
    const r = el.getBoundingClientRect();
    if (!r.width || !r.height) return;
    const k = Math.min(1.25, Math.max(0.12, Math.min(r.width / graph.width, r.height / graph.height) * 0.95));
    setView({ k, x: (r.width - graph.width * k) / 2, y: Math.max(8, (r.height - graph.height * k) / 2) });
  }, [graph]);

  useLayoutEffect(() => {
    if (graph && active && !fitted.current) {
      fitted.current = true;
      fit();
    }
  }, [graph, active, fit]);

  // Wheel: pinch / ⌘-scroll zooms around the cursor, plain scroll pans.
  useEffect(() => {
    const el = host.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const r = el.getBoundingClientRect();
      if (e.ctrlKey || e.metaKey) {
        const mx = e.clientX - r.left;
        const my = e.clientY - r.top;
        setView((v) => {
          const k = Math.min(3, Math.max(0.08, v.k * Math.exp(-e.deltaY * 0.01)));
          return { k, x: mx - ((mx - v.x) * k) / v.k, y: my - ((my - v.y) * k) / v.k };
        });
      } else {
        setView((v) => ({ ...v, x: v.x - e.deltaX, y: v.y - e.deltaY }));
      }
    };
    el.addEventListener('wheel', onWheel, { passive: false });
    return () => el.removeEventListener('wheel', onWheel);
  }, []);

  const zoomBy = (f: number) => {
    const el = host.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const mx = r.width / 2;
    const my = r.height / 2;
    setView((v) => {
      const k = Math.min(3, Math.max(0.08, v.k * f));
      return { k, x: mx - ((mx - v.x) * k) / v.k, y: my - ((my - v.y) * k) / v.k };
    });
  };

  const onPointerDown = (e: React.PointerEvent) => {
    if ((e.target as Element).closest('[data-table]')) return;
    const start = { x: e.clientX, y: e.clientY };
    const origin = { ...view };
    setSelected(null);
    const move = (ev: PointerEvent) => setView((v) => ({ ...v, x: origin.x + ev.clientX - start.x, y: origin.y + ev.clientY - start.y }));
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      host.current?.classList.remove('is-panning');
    };
    host.current?.classList.add('is-panning');
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  };

  const focusTable = (name: string) => {
    const el = host.current;
    const n = graph?.nodes.get(name);
    if (!el || !n) return;
    const r = el.getBoundingClientRect();
    const k = Math.max(view.k, 0.8);
    setView({ k, x: r.width / 2 - (n.x + n.w / 2) * k, y: r.height / 2 - (n.y + n.h / 2) * k });
    setSelected(name);
  };

  const focus = hover ?? selected;
  const related = useMemo(() => {
    if (!focus || !graph) return null;
    const s = new Set([focus]);
    for (const e of graph.edges) {
      if (e.from === focus) s.add(e.to);
      if (e.to === focus) s.add(e.from);
    }
    return s;
  }, [focus, graph]);

  const matches = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q || !graph) return null;
    return new Set([...graph.nodes.keys()].filter((n) => n.toLowerCase().includes(q)));
  }, [query, graph]);

  const serialize = () => {
    const svg = svgRef.current;
    if (!svg || !graph) return null;
    const clone = svg.cloneNode(true) as SVGSVGElement;
    clone.setAttribute('xmlns', 'http://www.w3.org/2000/svg');
    clone.setAttribute('width', String(graph.width));
    clone.setAttribute('height', String(graph.height));
    clone.setAttribute('viewBox', `0 0 ${graph.width} ${graph.height}`);
    clone.querySelector('[data-viewport]')?.removeAttribute('transform');
    const bg = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
    bg.setAttribute('width', '100%');
    bg.setAttribute('height', '100%');
    bg.setAttribute('fill', palette.bg);
    clone.insertBefore(bg, clone.firstChild);
    return new XMLSerializer().serializeToString(clone);
  };

  const exportAs = async (format: 'svg' | 'png') => {
    const text = serialize();
    if (!text || !graph) return;
    const path = await pickSaveFile(`Export diagram as ${format.toUpperCase()}`, `${tab.schema}-diagram.${format}`, [{ name: format.toUpperCase(), extensions: [format] }]);
    if (!path) return;
    try {
      if (format === 'svg') {
        await call('fs.writeText', { path, contents: text });
      } else {
        const scale = Math.min(2, 16000 / Math.max(graph.width, graph.height));
        const img = new Image();
        img.src = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(text);
        await img.decode();
        const canvas = document.createElement('canvas');
        canvas.width = Math.round(graph.width * scale);
        canvas.height = Math.round(graph.height * scale);
        const ctx = canvas.getContext('2d')!;
        ctx.scale(scale, scale);
        ctx.drawImage(img, 0, 0);
        const data = canvas.toDataURL('image/png').split(',')[1];
        await call('fs.writeBase64', { path, data });
      }
      toast('success', 'Diagram exported', path);
    } catch (e) {
      toast('error', 'Couldn’t export the diagram', errorMessage(e));
    }
  };

  const nodes = graph ? [...graph.nodes.values()] : [];
  const dimmed = (name: string) => (related ? !related.has(name) : matches ? !matches.has(name) : false);

  return (
    <div className="erd">
      <div className="erd-toolbar">
        <div className="erd-search">
          <Search size={13} />
          <input
            className="input input-sm"
            value={query}
            placeholder="Find table"
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && matches?.size) focusTable([...matches][0]);
            }}
          />
        </div>
        <Segmented
          size="sm"
          value={compact ? 'keys' : 'all'}
          onChange={(v) => setKeysOnly(v === 'keys')}
          options={[
            { value: 'all', label: 'All columns' },
            { value: 'keys', label: 'Keys only' },
          ]}
        />
        <span className="dim erd-count">
          {graph ? `${graph.nodes.size} tables · ${graph.edges.length} relationships` : ''}
        </span>
        <div className="toolbar-spacer" />
        <IconButton label="Zoom out" onClick={() => zoomBy(1 / 1.25)}>
          <Minus size={14} />
        </IconButton>
        <span className="erd-zoom mono">{Math.round(view.k * 100)}%</span>
        <IconButton label="Zoom in" onClick={() => zoomBy(1.25)}>
          <Plus size={14} />
        </IconButton>
        <IconButton label="Fit to window" onClick={fit}>
          <Maximize2 size={14} />
        </IconButton>
        <IconButton label="Reload" onClick={() => void load()}>
          <RefreshCw size={14} className={clsx(loading && 'spin')} />
        </IconButton>
        <Button size="sm" variant="ghost" disabled={!graph} onClick={() => graph && copyText(mermaid(nodes, graph.edges)).then(() => toast('success', 'Copied as Mermaid', 'Paste it into GitHub, Notion or docs.'))}>
          <Copy size={13} /> Mermaid
        </Button>
        <Button size="sm" variant="ghost" disabled={!graph} onClick={() => exportAs('svg')}>
          <Download size={13} /> SVG
        </Button>
        <Button size="sm" variant="ghost" disabled={!graph} onClick={() => exportAs('png')}>
          <Download size={13} /> PNG
        </Button>
      </div>
      <div ref={host} className="erd-canvas" onPointerDown={onPointerDown} style={{ background: palette.bg }}>
        {loading && !data && (
          <div className="erd-center">
            <Spinner /> Reading schema…
          </div>
        )}
        {error && (
          <div className="erd-center">
            <EmptyState title="Couldn’t load the diagram">{error}</EmptyState>
          </div>
        )}
        {graph && graph.nodes.size === 0 && (
          <div className="erd-center">
            <EmptyState title={`No tables in ${tab.schema}`}>Create a table and reload.</EmptyState>
          </div>
        )}
        {graph && graph.nodes.size > 0 && (
          <svg ref={svgRef} className="erd-svg" width="100%" height="100%" fontFamily="ui-monospace, SFMono-Regular, Menlo, monospace" fontSize={11.5}>
            <defs>
              {/* "one" bars sit just outside the referenced table; the "many" crow's foot spreads against the child table. */}
              {(['', '-hot'] as const).map((hot) => (
                <g key={hot}>
                  <marker id={`one${hot}-${tab.id}`} viewBox="0 0 14 14" refX="13" refY="7" markerWidth="14" markerHeight="14" orient="auto">
                    <path d="M 5 2 L 5 12 M 9 2 L 9 12" stroke={hot ? palette.accent : palette.edge} strokeWidth="1.4" fill="none" />
                  </marker>
                  <marker id={`many${hot}-${tab.id}`} viewBox="0 0 14 14" refX="0.5" refY="7" markerWidth="14" markerHeight="14" orient="auto">
                    <path d="M 0.5 1.5 L 11 7 L 0.5 12.5 M 0.5 7 L 11 7" stroke={hot ? palette.accent : palette.edge} strokeWidth="1.4" fill="none" />
                  </marker>
                </g>
              ))}
            </defs>
            <g data-viewport transform={`translate(${view.x} ${view.y}) scale(${view.k})`}>
              {graph.edges.map((e, i) => {
                const a = graph.nodes.get(e.from)!;
                const b = graph.nodes.get(e.to)!;
                const p = edgePath(a, e.fromCol, b, e.toCol);
                const hot = !!focus && (e.from === focus || e.to === focus);
                const faded = !!related && !hot;
                return (
                  <path
                    key={`${e.name}:${i}`}
                    d={p.d}
                    fill="none"
                    stroke={hot ? palette.accent : palette.edge}
                    strokeWidth={hot ? 1.8 : 1.2}
                    opacity={faded ? 0.15 : 1}
                    markerStart={`url(#many${hot ? '-hot' : ''}-${tab.id})`}
                    markerEnd={`url(#one${hot ? '-hot' : ''}-${tab.id})`}
                  >
                    <title>{`${e.from}.${e.fromCol} → ${e.to}.${e.toCol}${e.name ? ` (${e.name})` : ''}`}</title>
                  </path>
                );
              })}
              {nodes.map((n) => {
                const isSel = selected === n.name;
                return (
                  <g
                    key={n.name}
                    data-table={n.name}
                    transform={`translate(${n.x} ${n.y})`}
                    opacity={dimmed(n.name) ? 0.28 : 1}
                    onPointerEnter={() => setHover(n.name)}
                    onPointerLeave={() => setHover((h) => (h === n.name ? null : h))}
                    onClick={() => setSelected(n.name)}
                    onDoubleClick={() => openTable(ws.sessionId, tab.schema, { name: n.name, kind: n.kind === 'view' ? 'view' : 'table' })}
                    style={{ cursor: 'pointer' }}
                  >
                    <rect width={n.w} height={n.h} rx={8} fill={palette.card} stroke={isSel || matches?.has(n.name) ? palette.accent : palette.border} strokeWidth={isSel ? 2 : 1} />
                    <path d={`M 0 8 a 8 8 0 0 1 8 -8 h ${n.w - 16} a 8 8 0 0 1 8 8 v ${HEAD_H - 8} h ${-n.w} z`} fill={palette.head} />
                    <line x1={0} x2={n.w} y1={HEAD_H} y2={HEAD_H} stroke={palette.border} />
                    <text x={12} y={HEAD_H / 2 + 4} fill={palette.text} fontWeight={700} fontFamily="-apple-system, BlinkMacSystemFont, 'Segoe UI', Inter, sans-serif" fontSize={12.5}>
                      {n.name.length > 40 ? n.name.slice(0, 39) + '…' : n.name}
                    </text>
                    {n.kind === 'view' && (
                      <text x={n.w - 10} y={HEAD_H / 2 + 4} textAnchor="end" fill={palette.faint} fontSize={10}>
                        view
                      </text>
                    )}
                    {n.cols.length === 0 && (
                      <text x={12} y={HEAD_H + ROW_H / 2 + 4} fill={palette.faint} fontStyle="italic">
                        no key columns
                      </text>
                    )}
                    {n.cols.map((c, i) => (
                      <g key={c.name} transform={`translate(0 ${HEAD_H + i * ROW_H})`}>
                        {c.pk && <path d="M 11 7 h 6 v 7 h -6 z M 12.5 7 v -2 a 1.5 1.5 0 0 1 3 0 v 2" fill="none" stroke={palette.accent} strokeWidth={1.2} transform="translate(0 0.5)" />}
                        {!c.pk && c.fk && <path d="M 10 11 l 3 -3 M 14 9 l 2 -2 a 1.6 1.6 0 0 1 2.3 2.3 l -2 2 M 13 12 l -2 2 a 1.6 1.6 0 0 1 -2.3 -2.3 l 2 -2" fill="none" stroke={palette.dim} strokeWidth={1.1} />}
                        <text x={26} y={ROW_H / 2 + 4} fill={c.pk ? palette.text : palette.dim} fontWeight={c.pk ? 600 : 400}>
                          {c.name}
                        </text>
                        <text x={n.w - 10} y={ROW_H / 2 + 4} textAnchor="end" fill={palette.faint}>
                          {c.type.length > 22 ? c.type.slice(0, 21) + '…' : c.type}
                        </text>
                      </g>
                    ))}
                  </g>
                );
              })}
            </g>
          </svg>
        )}
        {graph && graph.nodes.size > 0 && (
          <div className="erd-legend">
            <span>
              <KeyRound size={11} /> primary key
            </span>
            <span>
              <Link2 size={11} /> foreign key
            </span>
            <span className="dim">Scroll to pan · ⌘/pinch to zoom · double-click a table to open it</span>
          </div>
        )}
      </div>
    </div>
  );
}
