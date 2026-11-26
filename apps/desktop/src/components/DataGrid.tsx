import { useVirtualizer } from '@tanstack/react-virtual';
import clsx from 'clsx';
import { ArrowDown, ArrowUp, ArrowUpRight, Copy, Filter as FilterIcon, Rows3, Trash2, CopyPlus, PanelRight, Ban, KeyRound } from 'lucide-react';
import { memo, useCallback, useEffect, useImperativeHandle, useMemo, useRef, useState, forwardRef } from 'react';
import { isBinaryType, isNumericType } from '../lib/dialect';
import { toDelimited, toInserts, toJSON, toMarkdown, toTSV } from '../lib/exporters';
import { preview } from '../lib/format';
import { copyText, mod } from '../lib/platform';
import type { Sort, StagedChanges } from '../lib/sqlgen';
import type { Cell, Column, Engine, ForeignKeyDef } from '../lib/types';
import { useMenu, type MenuItem } from './ui';
import { useStore } from '../state/store';

export interface GridSelection {
  r1: number;
  c1: number;
  r2: number;
  c2: number;
}

export interface DataGridHandle {
  focus(): void;
  selectedRows(): number[];
  selection(): GridSelection | null;
  scrollToRow(r: number): void;
  editCell(r: number, c: number): void;
}

interface Props {
  columns: Column[];
  rows: Cell[][];
  engine: Engine;
  active: boolean;
  rowOffset?: number;
  editable?: boolean;
  readOnlyReason?: string;
  changes?: StagedChanges;
  pkColumns?: string[];
  /** Columns computed by the database (generated / identity ALWAYS): never editable. */
  readOnlyColumns?: Set<string>;
  fks?: Map<string, ForeignKeyDef>;
  sort?: Sort[];
  copyContext?: { schema: string; table: string };
  onSort?(column: string, additive: boolean, dir?: 'asc' | 'desc'): void;
  onEdit?(row: number, col: number, value: Cell): void;
  onDeleteRows?(rows: number[]): void;
  onDuplicateRows?(rows: number[]): void;
  onFollowFk?(fk: ForeignKeyDef, row: number): void;
  onFilterBy?(column: string, value: Cell): void;
  onActiveRowChange?(row: number | null): void;
}

const HEADER_H = 30;

function normalize(s: GridSelection) {
  return {
    top: Math.min(s.r1, s.r2),
    bottom: Math.max(s.r1, s.r2),
    left: Math.min(s.c1, s.c2),
    right: Math.max(s.c1, s.c2),
  };
}

function measureWidths(columns: Column[], rows: Cell[][], pk?: Set<string>): number[] {
  const sample = rows.slice(0, 60);
  return columns.map((c, i) => {
    // Header: name + type label + icons
    const header = c.name.length * 7.2 + Math.min(c.typeName.length, 16) * 6 + 30 + (pk?.has(c.name) ? 15 : 0);
    let len = 0;
    for (const r of sample) {
      const v = r[i];
      if (v !== null && v !== undefined) len = Math.max(len, Math.min(v.length, 60));
    }
    const content = len * 7.3 + 20;
    return Math.round(Math.min(420, Math.max(64, header, content)));
  });
}

interface RowProps {
  index: number;
  start: number;
  height: number;
  values: Cell[];
  widths: number[];
  columns: Column[];
  numeric: boolean[];
  rowLabel: string;
  status: 'normal' | 'deleted' | 'inserted';
  edited: Set<number> | undefined;
  defaults: Set<number> | undefined;
  selLeft: number;
  selRight: number;
  inSel: boolean;
  focusCol: number;
  fkCols: boolean[];
  totalWidth: number;
}

const Row = memo(function Row(p: RowProps) {
  return (
    <div
      className={clsx('grid-row', p.index % 2 === 1 && 'is-alt', p.status !== 'normal' && `is-${p.status}`)}
      style={{ transform: `translateY(${p.start + HEADER_H}px)`, height: p.height, width: p.totalWidth }}
      data-row={p.index}
    >
      <div className={clsx('grid-rownum', p.inSel && p.selLeft === 0 && p.selRight === p.columns.length - 1 && 'is-selected')} data-col={-1}>
        {p.status === 'inserted' ? '+' : p.rowLabel}
      </div>
      {p.columns.map((col, c) => {
        const v = p.values[c];
        const isDefault = p.defaults?.has(c);
        const selected = p.inSel && c >= p.selLeft && c <= p.selRight;
        return (
          <div
            key={c}
            data-col={c}
            className={clsx(
              'grid-cell',
              p.numeric[c] && 'is-num',
              v === null && !isDefault && 'is-null',
              isDefault && 'is-default',
              selected && 'is-selected',
              p.inSel && p.focusCol === c && 'is-focus',
              p.edited?.has(c) && 'is-edited',
            )}
            style={{ width: p.widths[c] }}
          >
            {isDefault ? 'DEFAULT' : v === null ? 'NULL' : v === undefined ? '' : preview(v, 200)}
            {p.fkCols[c] && v !== null && !isDefault && <span className="fk-link" data-fk={c}><ArrowUpRight size={11} /></span>}
          </div>
        );
      })}
    </div>
  );
});

export const DataGrid = forwardRef<DataGridHandle, Props>(function DataGrid(props, ref) {
  const {
    columns,
    rows,
    engine,
    active,
    rowOffset = 0,
    editable,
    changes,
    pkColumns,
    readOnlyColumns,
    fks,
    sort,
    copyContext,
    onSort,
    onEdit,
    onDeleteRows,
    onDuplicateRows,
    onFollowFk,
    onFilterBy,
    onActiveRowChange,
  } = props;
  const density = useStore((s) => s.settings.density);
  const rowHeight = density === 'comfortable' ? 30 : 25;
  const scrollRef = useRef<HTMLDivElement>(null);
  const [widths, setWidths] = useState<number[]>(() => measureWidths(columns, rows, new Set(pkColumns ?? [])));
  const colKey = columns.map((c) => c.name).join('\u0001');
  const [sel, setSel] = useState<GridSelection | null>(null);
  const [editing, setEditing] = useState<{ r: number; c: number; value: string; original: Cell } | null>(null);
  const dragging = useRef(false);
  const [menu, openMenu] = useMenu();
  const toast = useStore((s) => s.toast);

  useEffect(() => {
    setWidths(measureWidths(columns, rows, new Set(pkColumns ?? [])));
    setSel(null);
    setEditing(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [colKey, (pkColumns ?? []).join(',')]);

  const inserts = changes?.inserts ?? [];
  const total = rows.length + inserts.length;
  const numeric = useMemo(() => columns.map((c) => isNumericType(c.typeName)), [columns]);
  const fkCols = useMemo(() => columns.map((c) => !!fks?.has(c.name)), [columns, fks]);
  const totalWidth = useMemo(() => 52 + widths.reduce((a, b) => a + b, 0), [widths]);

  const valueAt = useCallback(
    (r: number, c: number): Cell => {
      if (r < rows.length) {
        const upd = changes?.updates.get(r);
        const name = columns[c]?.name;
        if (upd && name !== undefined && upd.has(name)) return upd.get(name)!;
        return rows[r]?.[c] ?? null;
      }
      const ins = inserts[r - rows.length];
      const name = columns[c]?.name;
      if (!ins || name === undefined) return null;
      return ins.values.has(name) ? ins.values.get(name)! : null;
    },
    [rows, changes, columns, inserts],
  );

  const virtualizer = useVirtualizer({
    count: total,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => rowHeight,
    overscan: 14,
  });

  useEffect(() => {
    virtualizer.measure();
  }, [rowHeight, virtualizer]);

  const focusGrid = () => scrollRef.current?.focus({ preventScroll: true });

  const scrollToCell = useCallback(
    (r: number, c: number) => {
      virtualizer.scrollToIndex(r, { align: 'auto' });
      const el = scrollRef.current;
      if (!el || c < 0) return;
      let x = 52;
      for (let i = 0; i < c; i++) x += widths[i];
      const w = widths[c] ?? 100;
      if (x - 52 < el.scrollLeft) el.scrollLeft = x - 52;
      else if (x + w > el.scrollLeft + el.clientWidth) el.scrollLeft = x + w - el.clientWidth;
    },
    [virtualizer, widths],
  );

  const selectedRows = useCallback((): number[] => {
    if (!sel) return [];
    const n = normalize(sel);
    const out: number[] = [];
    for (let r = n.top; r <= n.bottom; r++) out.push(r);
    return out;
  }, [sel]);

  const canEditCell = useCallback(
    (r: number, c: number) => {
      if (!editable || !onEdit) return false;
      if (r < rows.length && changes?.deletes.has(r)) return false;
      if (readOnlyColumns?.has(columns[c]?.name)) return false;
      return !isBinaryType(columns[c]?.typeName);
    },
    [editable, onEdit, rows.length, changes, columns, readOnlyColumns],
  );

  const startEdit = useCallback(
    (r: number, c: number, initial?: string) => {
      if (!canEditCell(r, c)) {
        if (editable === false && props.readOnlyReason) toast('info', 'Read-only', props.readOnlyReason);
        return;
      }
      const original = valueAt(r, c);
      scrollToCell(r, c);
      setEditing({ r, c, value: initial ?? original ?? '', original });
    },
    [canEditCell, valueAt, scrollToCell, editable, props.readOnlyReason, toast],
  );

  useImperativeHandle(
    ref,
    () => ({
      focus: focusGrid,
      selectedRows,
      selection: () => sel,
      scrollToRow: (r: number) => virtualizer.scrollToIndex(r, { align: 'center' }),
      editCell: (r: number, c: number) => {
        setSel({ r1: r, c1: c, r2: r, c2: c });
        setTimeout(() => startEdit(r, c), 0);
      },
    }),
    [selectedRows, sel, virtualizer, startEdit],
  );

  const activeRow = sel ? sel.r2 : null;
  useEffect(() => {
    onActiveRowChange?.(activeRow);
  }, [activeRow, onActiveRowChange]);

  const commitEdit = useCallback(
    (move?: 'down' | 'right' | 'left') => {
      if (!editing) return;
      const { r, c, value, original } = editing;
      setEditing(null);
      if (value !== (original ?? '')) {
        onEdit?.(r, c, value);
      }
      if (move) {
        const nr = move === 'down' ? Math.min(total - 1, r + 1) : r;
        const nc = move === 'right' ? Math.min(columns.length - 1, c + 1) : move === 'left' ? Math.max(0, c - 1) : c;
        setSel({ r1: nr, c1: nc, r2: nr, c2: nc });
        scrollToCell(nr, nc);
      }
      setTimeout(focusGrid, 0);
    },
    [editing, onEdit, total, columns.length, scrollToCell],
  );

  const copySelection = useCallback(
    async (format: 'tsv' | 'csv' | 'json' | 'sql' | 'md' | 'tsv-header' = 'tsv') => {
      if (!sel) return;
      const n = normalize(sel);
      const cols = columns.slice(n.left, n.right + 1);
      const data: Cell[][] = [];
      for (let r = n.top; r <= n.bottom; r++) {
        const row: Cell[] = [];
        for (let c = n.left; c <= n.right; c++) row.push(valueAt(r, c));
        data.push(row);
      }
      let text: string;
      if (format === 'csv') text = toDelimited(cols, data);
      else if (format === 'json') text = toJSON(cols, data);
      else if (format === 'sql') text = toInserts(engine, copyContext?.schema ?? '', copyContext?.table ?? '', cols, data);
      else if (format === 'md') text = toMarkdown(cols, data);
      else if (format === 'tsv-header') text = toTSV(cols, data);
      else if (data.length === 1 && data[0].length === 1) text = data[0][0] ?? '';
      else text = toTSV(null, data);
      await copyText(text);
      const cells = data.length * cols.length;
      toast('success', `Copied ${cells === 1 ? 'value' : `${data.length} × ${cols.length}`}`);
    },
    [sel, columns, valueAt, engine, copyContext, toast],
  );

  // Keyboard handling (only for the focused grid)
  const onKeyDown = (e: React.KeyboardEvent) => {
    if (editing || !total || !columns.length) return;
    const meta = e.metaKey || e.ctrlKey;
    const cur = sel ?? { r1: 0, c1: 0, r2: 0, c2: 0 };
    const move = (dr: number, dc: number, extend: boolean) => {
      const r = Math.max(0, Math.min(total - 1, cur.r2 + dr));
      const c = Math.max(0, Math.min(columns.length - 1, cur.c2 + dc));
      setSel(extend ? { ...cur, r2: r, c2: c } : { r1: r, c1: c, r2: r, c2: c });
      scrollToCell(r, c);
    };
    const page = Math.max(1, Math.floor((scrollRef.current?.clientHeight ?? 400) / rowHeight) - 1);
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        return move(meta ? total : 1, 0, e.shiftKey);
      case 'ArrowUp':
        e.preventDefault();
        return move(meta ? -total : -1, 0, e.shiftKey);
      case 'ArrowRight':
        e.preventDefault();
        return move(0, meta ? columns.length : 1, e.shiftKey);
      case 'ArrowLeft':
        e.preventDefault();
        return move(0, meta ? -columns.length : -1, e.shiftKey);
      case 'PageDown':
        e.preventDefault();
        return move(page, 0, e.shiftKey);
      case 'PageUp':
        e.preventDefault();
        return move(-page, 0, e.shiftKey);
      case 'Home':
        e.preventDefault();
        return move(meta ? -total : 0, -columns.length, e.shiftKey);
      case 'End':
        e.preventDefault();
        return move(meta ? total : 0, columns.length, e.shiftKey);
      case 'Tab':
        e.preventDefault();
        return move(0, e.shiftKey ? -1 : 1, false);
      case 'Enter':
      case 'F2':
        if (sel) {
          e.preventDefault();
          startEdit(sel.r2, sel.c2);
        }
        return;
      case 'Escape':
        if (sel) setSel({ r1: sel.r2, c1: sel.c2, r2: sel.r2, c2: sel.c2 });
        return;
      case 'Backspace':
      case 'Delete': {
        if (!sel || !editable) return;
        e.preventDefault();
        const n = normalize(sel);
        const wholeRows = n.left === 0 && n.right === columns.length - 1;
        if (meta || wholeRows) onDeleteRows?.(selectedRows());
        else startEdit(sel.r2, sel.c2, '');
        return;
      }
    }
    if (meta && e.key.toLowerCase() === 'c') {
      e.preventDefault();
      void copySelection(e.shiftKey ? 'tsv-header' : 'tsv');
      return;
    }
    if (meta && e.key.toLowerCase() === 'a') {
      e.preventDefault();
      setSel({ r1: 0, c1: 0, r2: total - 1, c2: columns.length - 1 });
      return;
    }
    if (meta && e.key.toLowerCase() === 'd' && editable) {
      e.preventDefault();
      onDuplicateRows?.(selectedRows());
      return;
    }
    if (!meta && !e.altKey && e.key.length === 1 && sel && editable) {
      e.preventDefault();
      startEdit(sel.r2, sel.c2, e.key);
    }
  };

  const cellFromEvent = (e: React.MouseEvent): { r: number; c: number; fk?: number } | null => {
    const target = e.target as HTMLElement;
    const fkEl = target.closest('[data-fk]') as HTMLElement | null;
    const cellEl = target.closest('[data-col]') as HTMLElement | null;
    const rowEl = target.closest('[data-row]') as HTMLElement | null;
    if (!cellEl || !rowEl) return null;
    return {
      r: Number(rowEl.dataset.row),
      c: Number(cellEl.dataset.col),
      fk: fkEl ? Number(fkEl.dataset.fk) : undefined,
    };
  };

  const onMouseDown = (e: React.MouseEvent) => {
    if (e.button !== 0) return;
    const hit = cellFromEvent(e);
    if (!hit) return;
    if (editing) commitEdit();
    if (hit.fk !== undefined && fks && onFollowFk) {
      const fk = fks.get(columns[hit.fk].name);
      if (fk) {
        e.preventDefault();
        onFollowFk(fk, hit.r);
        return;
      }
    }
    if (hit.c === -1) {
      // Row number: select whole rows
      setSel((s) =>
        e.shiftKey && s ? { r1: s.r1, c1: 0, r2: hit.r, c2: columns.length - 1 } : { r1: hit.r, c1: 0, r2: hit.r, c2: columns.length - 1 },
      );
    } else if (e.shiftKey && sel) {
      setSel({ ...sel, r2: hit.r, c2: hit.c });
    } else {
      setSel({ r1: hit.r, c1: hit.c, r2: hit.r, c2: hit.c });
    }
    dragging.current = true;
    const up = () => {
      dragging.current = false;
      window.removeEventListener('mouseup', up);
    };
    window.addEventListener('mouseup', up);
    focusGrid();
  };

  const onMouseMove = (e: React.MouseEvent) => {
    if (!dragging.current || !sel) return;
    const hit = cellFromEvent(e);
    if (!hit) return;
    const isRowSel = sel.c1 === 0 && sel.c2 === columns.length - 1 && hit.c === -1;
    if (isRowSel) {
      if (hit.r !== sel.r2) setSel({ ...sel, r2: hit.r });
    } else if (hit.c >= 0 && (hit.r !== sel.r2 || hit.c !== sel.c2)) {
      setSel({ ...sel, r2: hit.r, c2: hit.c });
    }
  };

  const onDoubleClick = (e: React.MouseEvent) => {
    const hit = cellFromEvent(e);
    if (!hit || hit.c < 0) return;
    startEdit(hit.r, hit.c);
  };

  const onContextMenu = (e: React.MouseEvent) => {
    const hit = cellFromEvent(e);
    if (!hit) return;
    e.preventDefault();
    let current = sel;
    if (!current || !(hit.r >= normalize(current).top && hit.r <= normalize(current).bottom && (hit.c === -1 || (hit.c >= normalize(current).left && hit.c <= normalize(current).right)))) {
      current = hit.c === -1 ? { r1: hit.r, c1: 0, r2: hit.r, c2: columns.length - 1 } : { r1: hit.r, c1: hit.c, r2: hit.r, c2: hit.c };
      setSel(current);
    }
    const c = hit.c >= 0 ? hit.c : 0;
    const col = columns[c];
    const value = valueAt(hit.r, c);
    const n = normalize(current);
    const rowsSel: number[] = [];
    for (let r = n.top; r <= n.bottom; r++) rowsSel.push(r);
    const fk = col ? fks?.get(col.name) : undefined;
    const items: MenuItem[] = [
      { label: 'Copy', icon: <Copy size={14} />, shortcut: `${mod}C`, onSelect: () => copySelection('tsv') },
      { label: 'Copy with headers', shortcut: `⇧${mod}C`, onSelect: () => copySelection('tsv-header') },
      { label: 'Copy as CSV', onSelect: () => copySelection('csv') },
      { label: 'Copy as JSON', onSelect: () => copySelection('json') },
      { label: 'Copy as SQL INSERT', onSelect: () => copySelection('sql') },
      { label: 'Copy as Markdown', onSelect: () => copySelection('md') },
      { separator: true },
    ];
    if (onFilterBy && hit.c >= 0) {
      items.push({
        label: value === null ? `Filter: ${col.name} IS NULL` : `Filter: ${col.name} = ${preview(value, 24)}`,
        icon: <FilterIcon size={14} />,
        onSelect: () => onFilterBy(col.name, value),
      });
    }
    if (fk && value !== null && onFollowFk) {
      items.push({
        label: `Open ${fk.refTable} where ${fk.refColumns.join(', ')} = ${preview(value, 16)}`,
        icon: <ArrowUpRight size={14} />,
        onSelect: () => onFollowFk(fk, hit.r),
      });
    }
    items.push({ label: 'Show in inspector', icon: <PanelRight size={14} />, onSelect: () => useStore.setState({ inspectorVisible: true }) });
    if (editable) {
      items.push(
        { separator: true },
        { label: 'Edit cell', shortcut: '↩', disabled: hit.c < 0 || !canEditCell(hit.r, c), onSelect: () => startEdit(hit.r, c) },
        {
          label: 'Set NULL',
          icon: <Ban size={14} />,
          disabled: hit.c < 0 || !canEditCell(hit.r, c),
          onSelect: () => {
            for (let r = n.top; r <= n.bottom; r++) for (let cc = n.left; cc <= n.right; cc++) if (canEditCell(r, cc)) onEdit?.(r, cc, null);
          },
        },
        { label: rowsSel.length > 1 ? `Duplicate ${rowsSel.length} rows` : 'Duplicate row', icon: <CopyPlus size={14} />, shortcut: `${mod}D`, onSelect: () => onDuplicateRows?.(rowsSel) },
        {
          label: rowsSel.length > 1 ? `Delete ${rowsSel.length} rows` : 'Delete row',
          icon: <Trash2 size={14} />,
          shortcut: `${mod}⌫`,
          danger: true,
          onSelect: () => onDeleteRows?.(rowsSel),
        },
      );
    }
    openMenu(e, items);
  };

  const onHeaderContext = (e: React.MouseEvent, c: number) => {
    const col = columns[c];
    openMenu(e, [
      { label: 'Sort ascending', icon: <ArrowUp size={14} />, disabled: !onSort, onSelect: () => onSort?.(col.name, false, 'asc') },
      { label: 'Sort descending', icon: <ArrowDown size={14} />, disabled: !onSort, onSelect: () => onSort?.(col.name, false, 'desc') },
      { separator: true },
      { label: 'Select column', icon: <Rows3 size={14} />, onSelect: () => total && setSel({ r1: 0, c1: c, r2: total - 1, c2: c }) },
      { label: 'Copy column name', icon: <Copy size={14} />, onSelect: () => copyText(col.name) },
      { label: 'Auto-fit width', onSelect: () => autofit(c) },
    ]);
  };

  const autofit = (c: number) => {
    let len = columns[c].name.length + 2;
    for (let r = 0; r < Math.min(rows.length, 500); r++) {
      const v = rows[r][c];
      if (v) len = Math.max(len, Math.min(v.length, 120));
    }
    setWidths((w) => w.map((x, i) => (i === c ? Math.min(900, Math.max(60, len * 7.4 + 22)) : x)));
  };

  const startResize = (e: React.MouseEvent, c: number) => {
    e.preventDefault();
    e.stopPropagation();
    const startX = e.clientX;
    const startW = widths[c];
    document.body.classList.add('is-resizing-x');
    const move = (ev: MouseEvent) => {
      const w = Math.max(48, startW + ev.clientX - startX);
      setWidths((ws) => ws.map((x, i) => (i === c ? w : x)));
    };
    const up = () => {
      document.body.classList.remove('is-resizing-x');
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', up);
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', up);
  };

  const n = sel ? normalize(sel) : null;
  const items = virtualizer.getVirtualItems();
  const pkSet = useMemo(() => new Set(pkColumns ?? []), [pkColumns]);

  // Editor overlay position
  let editorStyle: React.CSSProperties | undefined;
  if (editing) {
    let x = 52;
    for (let i = 0; i < editing.c; i++) x += widths[i];
    const w = Math.max(widths[editing.c], 220);
    const multiline = editing.value.includes('\n') || editing.value.length > 80;
    editorStyle = {
      left: x,
      top: HEADER_H + editing.r * rowHeight,
      width: w,
      height: multiline ? Math.max(rowHeight * 5, 120) : rowHeight,
    };
  }

  return (
    <div className={clsx('grid', !active && 'is-inactive')}>
      <div
        ref={scrollRef}
        className="grid-scroll"
        tabIndex={0}
        onKeyDown={onKeyDown}
        onMouseDown={onMouseDown}
        onMouseMove={onMouseMove}
        onDoubleClick={onDoubleClick}
        onContextMenu={onContextMenu}
      >
        <div className="grid-canvas" style={{ width: totalWidth, height: HEADER_H + virtualizer.getTotalSize() }}>
          <div className="grid-header" style={{ width: totalWidth, height: HEADER_H }}>
            <div className="grid-rownum grid-corner" onClick={() => total && setSel({ r1: 0, c1: 0, r2: total - 1, c2: columns.length - 1 })} />
            {columns.map((col, c) => {
              const s = sort?.findIndex((x) => x.column === col.name) ?? -1;
              const dir = s >= 0 ? sort![s].dir : null;
              return (
                <div
                  key={c}
                  className={clsx('grid-hcell', n && c >= n.left && c <= n.right && 'is-selected', numeric[c] && 'is-num')}
                  style={{ width: widths[c] }}
                  onMouseDown={(e) => e.stopPropagation()}
                  onClick={(e) => onSort?.(col.name, e.shiftKey)}
                  onContextMenu={(e) => {
                    e.stopPropagation();
                    onHeaderContext(e, c);
                  }}
                  title={`${col.name}${col.typeName ? ` · ${col.typeName}` : ''}`}
                >
                  {pkSet.has(col.name) && <KeyRound size={11} className="pk-icon" />}
                  <span className="hcell-name">{col.name}</span>
                  {col.typeName && <span className="hcell-type">{col.typeName}</span>}
                  {dir && (
                    <span className="hcell-sort">
                      {dir === 'asc' ? <ArrowUp size={11} /> : <ArrowDown size={11} />}
                      {sort!.length > 1 && <sup>{s + 1}</sup>}
                    </span>
                  )}
                  <span className="hcell-resize" onMouseDown={(e) => startResize(e, c)} onDoubleClick={(e) => { e.stopPropagation(); autofit(c); }} onClick={(e) => e.stopPropagation()} />
                </div>
              );
            })}
          </div>
          {items.map((vi) => {
            const r = vi.index;
            const isInsert = r >= rows.length;
            const values = isInsert ? columns.map((_, c) => valueAt(r, c)) : changes?.updates.has(r) ? columns.map((_, c) => valueAt(r, c)) : rows[r];
            let edited: Set<number> | undefined;
            let defaults: Set<number> | undefined;
            if (!isInsert && changes?.updates.has(r)) {
              const u = changes.updates.get(r)!;
              edited = new Set(columns.map((col, c) => (u.has(col.name) ? c : -1)).filter((c) => c >= 0));
            }
            if (isInsert) {
              const ins = inserts[r - rows.length];
              edited = new Set(columns.map((col, c) => (ins.values.has(col.name) ? c : -1)).filter((c) => c >= 0));
              defaults = new Set(columns.map((col, c) => (!ins.values.has(col.name) ? c : -1)).filter((c) => c >= 0));
            }
            const inSel = !!n && r >= n.top && r <= n.bottom;
            return (
              <Row
                key={vi.key}
                index={r}
                start={vi.start}
                height={rowHeight}
                values={values}
                widths={widths}
                columns={columns}
                numeric={numeric}
                rowLabel={String(rowOffset + r + 1)}
                status={isInsert ? 'inserted' : changes?.deletes.has(r) ? 'deleted' : 'normal'}
                edited={edited}
                defaults={defaults}
                selLeft={n?.left ?? -1}
                selRight={n?.right ?? -1}
                inSel={inSel}
                focusCol={inSel && sel && sel.r2 === r ? sel.c2 : -1}
                fkCols={fkCols}
                totalWidth={totalWidth}
              />
            );
          })}
          {editing && (
            <textarea
              className="grid-editor"
              style={editorStyle}
              autoFocus
              value={editing.value}
              spellCheck={false}
              onChange={(e) => setEditing({ ...editing, value: e.target.value })}
              onFocus={(e) => {
                const el = e.currentTarget;
                el.setSelectionRange(el.value.length, el.value.length);
              }}
              onBlur={() => commitEdit()}
              onMouseDown={(e) => e.stopPropagation()}
              onDoubleClick={(e) => e.stopPropagation()}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === 'Escape') {
                  e.preventDefault();
                  setEditing(null);
                  setTimeout(focusGrid, 0);
                } else if (e.key === 'Enter' && !e.shiftKey && !e.altKey) {
                  e.preventDefault();
                  commitEdit('down');
                } else if (e.key === 'Tab') {
                  e.preventDefault();
                  commitEdit(e.shiftKey ? 'left' : 'right');
                }
              }}
            />
          )}
        </div>
        {total === 0 && <div className="grid-empty">No rows</div>}
      </div>
      {menu}
    </div>
  );
});
