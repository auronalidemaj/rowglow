import clsx from 'clsx';
import {
  ChevronLeft,
  ChevronRight,
  ChevronsLeft,
  ChevronsRight,
  Download,
  Filter as FilterIcon,
  Plus,
  RefreshCw,
  Trash2,
  Undo2,
  Check,
  Code2,
  Lock,
  TerminalSquare,
  AlertTriangle,
} from 'lucide-react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { isComputedColumn, quoteIdent, sqlValue } from '../lib/dialect';
import { toDelimited, toInserts, toJSON } from '../lib/exporters';
import { compactCount, formatCount, formatDuration, plural, uid } from '../lib/format';
import { useHotkeys } from '../lib/hotkeys';
import { copyText, mod, pickSaveFile } from '../lib/platform';
import { call, errorMessage } from '../lib/rpc';
import {
  buildChanges,
  buildCount,
  buildOrderBy,
  buildSelect,
  buildWhere,
  changeCount,
  emptyChanges,
  NoKeyError,
  type Filter,
  type Sort,
  type StagedChanges,
} from '../lib/sqlgen';
import type { Cell, ForeignKeyDef, QueryResult, StatementOutcome, TableStructure } from '../lib/types';
import { useInspect } from '../state/inspect';
import { useStore, type TableTab, type Workspace } from '../state/store';
import { DataGrid, type DataGridHandle } from './DataGrid';
import { FilterBar, newFilter } from './FilterBar';
import { CodeView } from './SqlEditor';
import { StructureView } from './StructureView';
import { Button, IconButton, Modal, Segmented, Spinner, useMenu } from './ui';

function cloneChanges(c: StagedChanges): StagedChanges {
  return {
    updates: new Map([...c.updates].map(([k, v]) => [k, new Map(v)])),
    deletes: new Set(c.deletes),
    inserts: c.inserts.map((i) => ({ key: i.key, values: new Map(i.values) })),
  };
}

const PAGE_SIZES = [100, 300, 1000, 5000];

export function TableView({ ws, tab, active }: { ws: Workspace; tab: TableTab; active: boolean }) {
  const engine = ws.connection.engine;
  const sessionId = ws.sessionId;
  const settings = useStore((s) => s.settings);
  const { toast, updateTab, openQuery, openTable, confirm, handleLimitError } = useStore.getState();
  const license = useStore((s) => s.license);

  const [structure, setStructure] = useState<TableStructure | null>(null);
  const [data, setData] = useState<QueryResult | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [offset, setOffset] = useState(0);
  const [pageSize, setPageSize] = useState(settings.pageSize);
  const [sort, setSort] = useState<Sort[]>([]);
  const [filters, setFilters] = useState<Filter[]>(tab.initialFilters ?? []);
  const [showFilters, setShowFilters] = useState(!!tab.initialFilters?.length);
  const [rawMode, setRawMode] = useState(false);
  const [rawWhere, setRawWhere] = useState('');
  const [applied, setApplied] = useState<{ filters: Filter[]; raw?: string }>({ filters: tab.initialFilters ?? [] });
  const [count, setCount] = useState<number | null>(null);
  const [counting, setCounting] = useState(false);
  const [changes, setChanges] = useState<StagedChanges>(emptyChanges);
  const [lastSql, setLastSql] = useState('');
  const [review, setReview] = useState<string[] | null>(null);
  const [reviewWarning, setReviewWarning] = useState<string | null>(null);
  const [committing, setCommitting] = useState(false);
  const [commitError, setCommitError] = useState<{ index: number; message: string } | null>(null);
  const [activeRow, setActiveRow] = useState<number | null>(null);
  const [filterFocus, setFilterFocus] = useState(0);
  const grid = useRef<DataGridHandle>(null);
  const [exportMenu, openExportMenu] = useMenu();
  const reqId = useRef(0);

  const isTable = tab.objectKind === 'table';
  const pk = structure?.primaryKey ?? [];
  const rows = data?.rows ?? [];
  const columns = data?.columns ?? [];
  const colMeta = useMemo(() => {
    const defs = new Map(structure?.columns.map((c) => [c.name, c.dataType]) ?? []);
    return columns.map((c) => ({ name: c.name, type: defs.get(c.name) || c.typeName }));
  }, [columns, structure]);
  const gridColumns = useMemo(() => {
    const defs = new Map(structure?.columns.map((c) => [c.name, c.dataType]) ?? []);
    return columns.map((c) => ({ name: c.name, typeName: defs.get(c.name) || c.typeName }));
  }, [columns, structure]);
  const readOnlyColumns = useMemo(
    () => new Set((structure?.columns ?? []).filter((c) => isComputedColumn(c.extra)).map((c) => c.name)),
    [structure],
  );
  const fks = useMemo(() => {
    const m = new Map<string, ForeignKeyDef>();
    for (const fk of structure?.foreignKeys ?? []) if (fk.columns.length === 1) m.set(fk.columns[0], fk);
    return m;
  }, [structure]);

  const readOnlyReason = ws.info.readOnly
    ? 'This connection is read-only.'
    : !isTable
      ? 'Views can’t be edited directly.'
      : structure && !pk.length
        ? 'This table has no primary key, so Rowglow can’t safely target rows for edits.'
        : undefined;
  const editable = !readOnlyReason && !!structure;
  const nChanges = changeCount(changes);
  const estimate = ws.objects.find((o) => o.name === tab.name)?.rowsEstimate;

  // Structure (for PKs, FKs and column types)
  useEffect(() => {
    let cancelled = false;
    call<TableStructure>('meta.structure', { sessionId, schema: tab.schema, table: tab.name })
      .then((s) => !cancelled && setStructure(s))
      .catch(() => !cancelled && setStructure({ columns: [], primaryKey: [], indexes: [], foreignKeys: [], constraints: [], triggers: [], referencedBy: [] }));
    return () => {
      cancelled = true;
    };
  }, [sessionId, tab.schema, tab.name, ws.refreshKey]);

  const load = useCallback(
    async (opts: { keepChanges?: boolean } = {}) => {
      if (!structure) return;
      const id = ++reqId.current;
      setLoading(true);
      setError(null);
      const where = buildWhere(engine, applied.filters, colMeta.length ? colMeta : structure.columns.map((c) => ({ name: c.name, type: c.dataType })), applied.raw);
      const orderBy = buildOrderBy(engine, sort, structure.primaryKey);
      const sql = buildSelect(engine, tab.schema, tab.name, { where, orderBy, limit: pageSize, offset });
      setLastSql(sql);
      try {
        const [out] = await call<StatementOutcome[]>('query.execute', {
          sessionId,
          statements: [sql],
          limit: pageSize,
          channel: 'aux',
        });
        if (id !== reqId.current) return;
        if (out.error) {
          setError(out.error.message);
          setData(null);
        } else {
          setData(out.result!);
          if (!opts.keepChanges) setChanges(emptyChanges());
        }
      } catch (e) {
        if (id === reqId.current) setError(errorMessage(e));
      } finally {
        if (id === reqId.current) setLoading(false);
      }
      // Row count
      const big = (estimate ?? 0) > 2_000_000 && !where;
      if (big) {
        setCount(null);
        return;
      }
      setCounting(true);
      try {
        const [c] = await call<StatementOutcome[]>('query.execute', {
          sessionId,
          statements: [buildCount(engine, tab.schema, tab.name, where)],
          channel: 'aux',
          limit: 1,
        });
        if (id === reqId.current) setCount(c.result ? Number(c.result.rows[0]?.[0] ?? 0) : null);
      } finally {
        if (id === reqId.current) setCounting(false);
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [structure, applied, sort, pageSize, offset, sessionId, tab.schema, tab.name, engine, estimate],
  );

  // Never reload underneath staged edits: rows are matched to edits by
  // position, so a silent reload could lose work. Defer until commit/discard.
  const nChangesRef = useRef(0);
  nChangesRef.current = nChanges;
  const staleRef = useRef(false);
  useEffect(() => {
    if (tab.view !== 'data') return;
    if (nChangesRef.current > 0) {
      staleRef.current = true;
      return;
    }
    staleRef.current = false;
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [structure, applied, sort, pageSize, offset, tab.view]);

  /** Runs a navigation (page, sort, filter…) after confirming staged edits may be discarded. */
  const go = async (fn: () => void) => {
    if (nChangesRef.current > 0) {
      const ok = await confirm({
        title: `Discard ${plural(nChangesRef.current, 'staged change')}?`,
        message: `Commit (${mod}S) or discard your edits before changing page, sorting or filters.`,
        confirmLabel: 'Discard and continue',
        danger: true,
      });
      if (!ok) return;
      setChanges(emptyChanges());
      setCommitError(null);
    }
    fn();
  };

  // Tab dirty flag
  useEffect(() => {
    if (!!nChanges !== !!tab.dirty) updateTab(sessionId, tab.id, { dirty: nChanges > 0, preview: false });
  }, [nChanges, tab.dirty, tab.id, sessionId, updateTab]);

  const exactCount = async () => {
    setCounting(true);
    const where = buildWhere(engine, applied.filters, colMeta, applied.raw);
    try {
      const [c] = await call<StatementOutcome[]>('query.execute', {
        sessionId,
        statements: [buildCount(engine, tab.schema, tab.name, where)],
        channel: 'aux',
        limit: 1,
      });
      setCount(c.result ? Number(c.result.rows[0]?.[0] ?? 0) : null);
    } finally {
      setCounting(false);
    }
  };

  // ------------------------------------------------------------- editing
  const onEdit = useCallback(
    (r: number, c: number, value: Cell) => {
      const col = columns[c]?.name;
      if (col === undefined) return;
      setChanges((prev) => {
        const next = cloneChanges(prev);
        if (r < rows.length) {
          const orig = rows[r][c];
          const m = next.updates.get(r) ?? new Map<string, Cell>();
          if (value === orig) m.delete(col);
          else m.set(col, value);
          if (m.size) next.updates.set(r, m);
          else next.updates.delete(r);
        } else {
          const ins = next.inserts[r - rows.length];
          if (ins) ins.values.set(col, value);
        }
        return next;
      });
      setCommitError(null);
    },
    [columns, rows],
  );

  const addRow = () => {
    if (!editable) {
      if (readOnlyReason) toast('info', 'Read-only', readOnlyReason);
      return;
    }
    setChanges((prev) => {
      const next = cloneChanges(prev);
      next.inserts.push({ key: uid(), values: new Map() });
      return next;
    });
    const newIndex = rows.length + changes.inserts.length;
    setTimeout(() => {
      grid.current?.scrollToRow(newIndex);
      const firstEditable = structure?.columns.findIndex((c) => !c.defaultValue && !c.extra) ?? 0;
      grid.current?.editCell(newIndex, Math.max(0, firstEditable));
    }, 30);
  };

  const deleteRows = useCallback(
    (sel: number[]) => {
      if (!editable || !sel.length) return;
      setChanges((prev) => {
        const next = cloneChanges(prev);
        const base = sel.filter((r) => r < rows.length);
        const ins = new Set(sel.filter((r) => r >= rows.length).map((r) => r - rows.length));
        const allDeleted = base.length > 0 && base.every((r) => next.deletes.has(r));
        for (const r of base) {
          if (allDeleted) next.deletes.delete(r);
          else next.deletes.add(r);
        }
        if (ins.size) next.inserts = next.inserts.filter((_, i) => !ins.has(i));
        return next;
      });
    },
    [editable, rows.length],
  );

  const duplicateRows = useCallback(
    (sel: number[]) => {
      if (!editable || !sel.length || !structure) return;
      const skip = new Set(
        structure.columns
          .filter((c) => c.isPrimaryKey || /nextval\(|auto_increment|identity/i.test(`${c.defaultValue ?? ''} ${c.extra ?? ''}`))
          .map((c) => c.name),
      );
      setChanges((prev) => {
        const next = cloneChanges(prev);
        for (const r of sel) {
          const values = new Map<string, Cell>();
          columns.forEach((col, c) => {
            if (skip.has(col.name)) return;
            const v =
              r < rows.length
                ? (prev.updates.get(r)?.has(col.name) ? prev.updates.get(r)!.get(col.name)! : rows[r][c])
                : (prev.inserts[r - rows.length]?.values.get(col.name) ?? null);
            values.set(col.name, v);
          });
          next.inserts.push({ key: uid(), values });
        }
        return next;
      });
    },
    [editable, structure, columns, rows],
  );

  const statements = (): string[] | null => {
    try {
      return buildChanges(engine, tab.schema, tab.name, colMeta, pk, rows, changes);
    } catch (e) {
      if (e instanceof NoKeyError) toast('error', 'Can’t build changes', e.message);
      else toast('error', 'Can’t build changes', errorMessage(e));
      return null;
    }
  };

  const commit = async (stmts: string[]) => {
    setCommitting(true);
    setCommitError(null);
    try {
      const res = await call<{ ok: boolean; failedIndex?: number; error?: { message: string } }>('query.transaction', {
        sessionId,
        statements: stmts,
        expectOneRow: true,
      });
      if (res.ok) {
        toast('success', `Committed ${plural(stmts.length, 'change')}`, `${tab.name} · in one transaction`);
        setReview(null);
        setChanges(emptyChanges());
        await load();
        void useStore.getState().loadObjects(sessionId);
      } else {
        const idx = res.failedIndex ?? 0;
        const message = res.error?.message ?? 'Unknown error';
        setCommitError({ index: idx, message });
        setReview(stmts);
        toast('error', 'Nothing was committed', message);
      }
    } catch (e) {
      toast('error', 'Commit failed', errorMessage(e));
    } finally {
      setCommitting(false);
    }
  };

  const saveChanges = async (forceReview = false) => {
    if (!nChanges) return;
    const stmts = statements();
    if (!stmts) return;
    const prod = ws.connection.env === 'production';
    // Deleting rows that other tables reference with ON DELETE CASCADE / SET NULL
    // changes more than what's on screen: always show the review sheet.
    const cascades = changes.deletes.size
      ? (structure?.referencedBy ?? []).filter((fk) => /CASCADE|SET NULL|SET DEFAULT/i.test(fk.onDelete))
      : [];
    const warning = cascades.length
      ? `Deleting these rows also changes other tables: ${cascades
          .map((fk) => `${fk.refTable} (ON DELETE ${fk.onDelete.toUpperCase()})`)
          .join(', ')}.`
      : null;
    setReviewWarning(warning);
    // Deletes can't be undone: always show exactly what will be removed first.
    if (forceReview || warning || changes.deletes.size > 0 || (prod && settings.confirmProdWrites)) {
      setReview(stmts);
      return;
    }
    await commit(stmts);
  };

  const discard = async () => {
    if (!nChanges) return;
    const ok = await confirm({ title: `Discard ${plural(nChanges, 'change')}?`, confirmLabel: 'Discard', danger: true });
    if (ok) {
      setChanges(emptyChanges());
      setCommitError(null);
      if (staleRef.current) {
        staleRef.current = false;
        void load();
      }
    }
  };

  const refresh = async () => {
    if (nChanges) {
      const ok = await confirm({
        title: 'Reload and discard changes?',
        message: `${plural(nChanges, 'staged change')} will be lost.`,
        confirmLabel: 'Reload',
        danger: true,
      });
      if (!ok) return;
      setChanges(emptyChanges());
    }
    void load();
  };

  // ------------------------------------------------------------- sorting / filtering
  const onSort = (column: string, additive: boolean, dir?: 'asc' | 'desc') =>
    go(() => {
    setOffset(0);
    setSort((prev) => {
      const cur = prev.find((s) => s.column === column);
      const nextDir: 'asc' | 'desc' | null = dir ?? (!cur ? 'asc' : cur.dir === 'asc' ? 'desc' : null);
      const rest = additive ? prev.filter((s) => s.column !== column) : [];
      if (!nextDir) return rest;
      return additive ? [...rest, { column, dir: nextDir }] : [{ column, dir: nextDir }];
    });
    });

  const applyFilters = () =>
    go(() => {
      setOffset(0);
      setApplied(rawMode ? { filters: [], raw: rawWhere } : { filters: filters.map((f) => ({ ...f })) });
    });

  const clearFilters = () =>
    go(() => {
      setFilters([newFilter()]);
      setRawWhere('');
      setOffset(0);
      setApplied({ filters: [] });
    });

  const filterBy = (column: string, value: Cell) =>
    go(() => {
      const f: Filter = { id: uid(), column, op: value === null ? 'is null' : '=', value: value ?? '', enabled: true };
      setRawMode(false);
      setFilters([f]);
      setShowFilters(true);
      setOffset(0);
      setApplied({ filters: [f] });
    });

  const toggleFilters = () => {
    if (!showFilters && !filters.length) setFilters([newFilter()]);
    setShowFilters((v) => !v);
    setFilterFocus((n) => n + 1);
  };

  const followFk = useCallback(
    (fk: ForeignKeyDef, r: number) => {
      const c = columns.findIndex((col) => col.name === fk.columns[0]);
      const value = r < rows.length ? (changes.updates.get(r)?.get(fk.columns[0]) ?? rows[r][c]) : null;
      if (value === null || value === undefined) return;
      openTable(sessionId, fk.refSchema, { name: fk.refTable, kind: 'table' }, {
        filters: [{ id: uid(), column: fk.refColumns[0], op: '=', value, enabled: true }],
      });
    },
    [columns, rows, changes, openTable, sessionId],
  );

  // ------------------------------------------------------------- inspector
  const setInspect = useInspect((s) => s.set);
  const clearInspect = useInspect((s) => s.clear);
  useEffect(() => {
    if (!active || tab.view !== 'data') return;
    if (activeRow === null || activeRow >= rows.length + changes.inserts.length) {
      clearInspect(tab.id);
      return;
    }
    const r = activeRow;
    const values = gridColumns.map((col, c) => {
      if (r < rows.length) {
        const u = changes.updates.get(r);
        return u?.has(col.name) ? u.get(col.name)! : rows[r][c];
      }
      const ins = changes.inserts[r - rows.length];
      return ins.values.has(col.name) ? ins.values.get(col.name)! : null;
    });
    setInspect({
      owner: tab.id,
      title: r < rows.length ? `${tab.name} · row ${offset + r + 1}` : `${tab.name} · new row`,
      columns: gridColumns,
      values,
      colDefs: structure?.columns,
      editable,
      fks,
      onChange: (c, v) => onEdit(r, c, v),
      onFollow: (fk) => followFk(fk, r),
    });
  }, [active, activeRow, rows, changes, gridColumns, structure, editable, fks, tab.id, tab.name, tab.view, offset, setInspect, clearInspect, onEdit, followFk]);

  useEffect(() => () => clearInspect(tab.id), [clearInspect, tab.id]);

  // ------------------------------------------------------------- export
  const exportLoaded = async (format: 'csv' | 'json' | 'sql') => {
    if (!data) return;
    const path = await pickSaveFile('Export rows', `${tab.name}.${format}`, [{ name: format.toUpperCase(), extensions: [format] }]);
    if (!path) return;
    const contents =
      format === 'csv'
        ? toDelimited(gridColumns, rows)
        : format === 'json'
          ? toJSON(gridColumns, rows)
          : toInserts(engine, tab.schema, tab.name, gridColumns, rows);
    try {
      await call('fs.writeText', { path, contents });
      toast('success', `Exported ${plural(rows.length, 'row')}`, path);
    } catch (e) {
      toast('error', 'Export failed', errorMessage(e));
    }
  };

  const exportAll = async (format: 'csv' | 'json' | 'sql') => {
    if (!license?.limits.tableExport) {
      useStore.getState().openModal({ kind: 'upgrade', reason: 'Exporting entire tables is part of Rowglow Pro. You can still export the rows on this page.' });
      return;
    }
    const path = await pickSaveFile(`Export ${tab.name}`, `${tab.name}.${format}`, [{ name: format.toUpperCase(), extensions: [format] }]);
    if (!path) return;
    const where = buildWhere(engine, applied.filters, colMeta, applied.raw);
    const orderBy = buildOrderBy(engine, sort, pk);
    const sql = buildSelect(engine, tab.schema, tab.name, { where, orderBy });
    toast('info', `Exporting ${tab.name}…`);
    try {
      const res = await call<{ rows: number }>('export.table', { sessionId, sql, path, format, schema: tab.schema, table: tab.name });
      toast('success', `Exported ${plural(res.rows, 'row')}`, path);
    } catch (e) {
      if (!handleLimitError(e)) toast('error', 'Export failed', errorMessage(e));
    }
  };

  // ------------------------------------------------------------- shortcuts
  useHotkeys(active && tab.view === 'data', {
    'mod+s': () => void saveChanges(false),
    'mod+shift+s': () => void saveChanges(true),
    'mod+r': () => void refresh(),
    'mod+f': () => toggleFilters(),
    'mod+shift+z': () => void discard(),
    'mod+i': () => addRow(),
    'mod+[': () => offset > 0 && void go(() => setOffset(Math.max(0, offset - pageSize))),
    'mod+]': () => rows.length === pageSize && void go(() => setOffset(offset + pageSize)),
  });

  useEffect(() => {
    if (active && tab.view === 'data') setTimeout(() => grid.current?.focus(), 20);
  }, [active, tab.view, data]);

  const setView = (view: TableTab['view']) => updateTab(sessionId, tab.id, { view, preview: false });

  const from = rows.length ? offset + 1 : 0;
  const to = offset + rows.length;
  const lastPage = count !== null ? Math.max(0, Math.floor((count - 1) / pageSize) * pageSize) : null;
  const activeFilterCount = applied.raw ? 1 : applied.filters.filter((f) => f.enabled && (f.value || f.op.includes('null'))).length;

  return (
    <div className="table-view">
      <div className="toolbar">
        <Segmented<TableTab['view']>
          size="sm"
          value={tab.view}
          onChange={setView}
          options={[
            { value: 'data', label: 'Data' },
            { value: 'structure', label: 'Structure' },
            { value: 'ddl', label: tab.objectKind === 'table' ? 'DDL' : 'Definition' },
          ]}
        />
        {tab.view === 'data' && (
          <>
            <div className="toolbar-sep" />
            <IconButton label={`Filter (${mod}F)`} active={showFilters} onClick={toggleFilters}>
              <FilterIcon size={15} />
              {activeFilterCount > 0 && <span className="dot-count">{activeFilterCount}</span>}
            </IconButton>
            <IconButton label={`Add row (${mod}I)`} onClick={addRow} disabled={!editable}>
              <Plus size={16} />
            </IconButton>
            <IconButton
              label={`Delete selected rows (${mod}⌫)`}
              disabled={!editable}
              onClick={() => deleteRows(grid.current?.selectedRows() ?? [])}
            >
              <Trash2 size={15} />
            </IconButton>
            <IconButton label={`Reload (${mod}R)`} onClick={refresh}>
              <RefreshCw size={14} className={clsx(loading && 'spin')} />
            </IconButton>
            {readOnlyReason && structure && (
              <span className="toolbar-note" title={readOnlyReason}>
                <Lock size={12} /> Read-only
              </span>
            )}
          </>
        )}
        <div className="toolbar-spacer" />
        {nChanges > 0 && tab.view === 'data' && (
          <div className="changes-bar">
            <span className="changes-count">{plural(nChanges, 'change')}</span>
            <Button size="sm" variant="ghost" onClick={discard} title={`Discard (${mod}⇧Z)`}>
              <Undo2 size={13} /> Discard
            </Button>
            <Button size="sm" onClick={() => saveChanges(true)} title={`Review SQL (${mod}⇧S)`}>
              <Code2 size={13} /> Review
            </Button>
            <Button size="sm" variant="primary" onClick={() => saveChanges(false)} disabled={committing} title={`Commit (${mod}S)`}>
              {committing ? <Spinner size={12} /> : <Check size={13} />} Commit
            </Button>
          </div>
        )}
      </div>

      {tab.view === 'data' && showFilters && (
        <FilterBar
          columns={gridColumns}
          filters={filters.length ? filters : [newFilter()]}
          raw={rawMode}
          rawWhere={rawWhere}
          focusKey={filterFocus}
          onChange={setFilters}
          onRawChange={(raw, where) => {
            setRawMode(raw);
            setRawWhere(where);
          }}
          onApply={applyFilters}
          onClear={clearFilters}
        />
      )}

      {tab.view === 'data' && (
        <div className="table-body">
          {error ? (
            <div className="error-panel">
              <AlertTriangle size={18} />
              <div>
                <div className="error-title">Query failed</div>
                <div className="error-msg">{error}</div>
                <pre className="error-sql">{lastSql}</pre>
              </div>
            </div>
          ) : !data ? (
            <div className="center-fill">
              <Spinner size={18} />
            </div>
          ) : (
            <DataGrid
              ref={grid}
              columns={gridColumns}
              rows={rows}
              engine={engine}
              active={active}
              rowOffset={offset}
              editable={editable}
              readOnlyReason={readOnlyReason}
              changes={changes}
              pkColumns={pk}
              readOnlyColumns={readOnlyColumns}
              fks={fks}
              sort={sort}
              copyContext={{ schema: tab.schema, table: tab.name }}
              onSort={onSort}
              onEdit={onEdit}
              onDeleteRows={deleteRows}
              onDuplicateRows={duplicateRows}
              onFollowFk={followFk}
              onFilterBy={filterBy}
              onActiveRowChange={setActiveRow}
            />
          )}
          {loading && data && <div className="loading-bar" />}
        </div>
      )}

      {tab.view === 'data' && (
        <div className="table-footer">
          <div className="pager">
            <IconButton label="First page" disabled={offset === 0} onClick={() => go(() => setOffset(0))}>
              <ChevronsLeft size={14} />
            </IconButton>
            <IconButton label={`Previous page (${mod}[)`} disabled={offset === 0} onClick={() => go(() => setOffset(Math.max(0, offset - pageSize)))}>
              <ChevronLeft size={14} />
            </IconButton>
            <span className="pager-label">
              {formatCount(from)}–{formatCount(to)} of{' '}
              {count !== null ? (
                formatCount(count)
              ) : counting ? (
                '…'
              ) : (
                <button className="link-btn" onClick={exactCount} title="Count rows exactly">
                  ~{compactCount(estimate ?? 0)}
                </button>
              )}
            </span>
            <IconButton
              label={`Next page (${mod}])`}
              disabled={rows.length < pageSize}
              onClick={() => go(() => setOffset(offset + pageSize))}
            >
              <ChevronRight size={14} />
            </IconButton>
            <IconButton
              label="Last page"
              disabled={lastPage === null || offset >= lastPage}
              onClick={() => lastPage !== null && go(() => setOffset(lastPage))}
            >
              <ChevronsRight size={14} />
            </IconButton>
            <select
              className="select select-sm"
              value={pageSize}
              onChange={(e) => {
                const size = Number(e.target.value);
                void go(() => {
                  setOffset(0);
                  setPageSize(size);
                });
              }}
              title="Rows per page"
            >
              {PAGE_SIZES.map((n) => (
                <option key={n} value={n}>
                  {n} rows
                </option>
              ))}
            </select>
          </div>
          <button
            className="footer-sql mono"
            title="Open this query in the SQL editor"
            onClick={() => openQuery(sessionId, { sql: lastSql + ';', title: tab.name })}
          >
            <TerminalSquare size={12} /> {lastSql}
          </button>
          <div className="footer-right">
            {data && <span className="dim">{formatDuration(data.durationMs)}</span>}
            <IconButton
              label="Export"
              onClick={(e) =>
                openExportMenu(e, [
                  { label: `This page as CSV (${formatCount(rows.length)} rows)`, onSelect: () => exportLoaded('csv') },
                  { label: 'This page as JSON', onSelect: () => exportLoaded('json') },
                  { label: 'This page as SQL INSERTs', onSelect: () => exportLoaded('sql') },
                  { separator: true },
                  { label: `Entire table as CSV${license?.limits.tableExport ? '' : ' · Pro'}`, onSelect: () => exportAll('csv') },
                  { label: `Entire table as JSON${license?.limits.tableExport ? '' : ' · Pro'}`, onSelect: () => exportAll('json') },
                  { label: `Entire table as SQL${license?.limits.tableExport ? '' : ' · Pro'}`, onSelect: () => exportAll('sql') },
                ])
              }
            >
              <Download size={14} />
            </IconButton>
          </div>
          {exportMenu}
        </div>
      )}

      {tab.view === 'structure' && <StructureView ws={ws} schema={tab.schema} table={tab.name} structure={structure} />}
      {tab.view === 'ddl' && <DdlPanel ws={ws} schema={tab.schema} name={tab.name} kind={tab.objectKind} />}

      {review && (
        <Modal
          title={
            <>
              Review changes · {tab.name}
              {ws.connection.env === 'production' && <span className="env-badge env-production">Production</span>}
            </>
          }
          onClose={() => setReview(null)}
          width={760}
          footer={
            <>
              <span className="dim modal-footer-note">
                {[
                  changes.updates.size ? plural([...changes.updates.keys()].filter((r) => !changes.deletes.has(r)).length, 'update') : '',
                  changes.inserts.length ? plural(changes.inserts.length, 'insert') : '',
                  changes.deletes.size ? plural(changes.deletes.size, 'delete') : '',
                ]
                  .filter(Boolean)
                  .join(' · ')}{' '}
                · one transaction, all or nothing
              </span>
              <Button variant="ghost" onClick={() => copyText(review.join(';\n') + ';').then(() => toast('success', 'SQL copied'))}>
                Copy SQL
              </Button>
              <Button onClick={() => setReview(null)}>Cancel</Button>
              <Button
                variant={ws.connection.env === 'production' || changes.deletes.size > 0 ? 'danger' : 'primary'}
                onClick={() => commit(review)}
                disabled={committing}
                autoFocus
              >
                {committing ? <Spinner size={12} /> : <Check size={13} />}{' '}
                {changes.deletes.size && changes.deletes.size === review.length
                  ? `Delete ${plural(changes.deletes.size, 'row')}`
                  : `Commit ${plural(review.length, 'statement')}`}
              </Button>
            </>
          }
        >
          {reviewWarning && <div className="callout callout-warn">{reviewWarning}</div>}
          {commitError && (
            <div className="callout callout-error">
              Statement {commitError.index + 1} failed: {commitError.message}. Nothing was committed.
            </div>
          )}
          <div className="review-code">
            <CodeView engine={engine} value={review.map((s) => s + ';').join('\n')} />
          </div>
        </Modal>
      )}
    </div>
  );
}

export function DdlPanel({ ws, schema, name, kind, signature }: { ws: Workspace; schema: string; name: string; kind: string; signature?: string }) {
  const [ddl, setDdl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const { toast, openQuery } = useStore.getState();
  useEffect(() => {
    setDdl(null);
    setError(null);
    call<string>('meta.ddl', { sessionId: ws.sessionId, schema, name, kind, signature: signature ?? null })
      .then(setDdl)
      .catch((e) => setError(errorMessage(e)));
  }, [ws.sessionId, schema, name, kind, signature, ws.refreshKey]);
  return (
    <div className="ddl-panel">
      <div className="ddl-actions">
        <Button size="sm" onClick={() => ddl && copyText(ddl).then(() => toast('success', 'Definition copied'))} disabled={!ddl}>
          Copy
        </Button>
        <Button size="sm" onClick={() => ddl && openQuery(ws.sessionId, { sql: ddl, title: name })} disabled={!ddl}>
          Open in editor
        </Button>
      </div>
      {error ? (
        <div className="error-panel">
          <AlertTriangle size={18} />
          <div className="error-msg">{error}</div>
        </div>
      ) : ddl === null ? (
        <div className="center-fill">
          <Spinner />
        </div>
      ) : (
        <div className="ddl-code">
          <CodeView engine={ws.connection.engine} value={ddl} fontSize={useStore.getState().settings.editorFontSize} />
        </div>
      )}
    </div>
  );
}

export function quickSelect(engine: Workspace['connection']['engine'], schema: string, table: string, col?: string, value?: Cell) {
  const where = col ? ` WHERE ${quoteIdent(engine, col)} = ${sqlValue(engine, value ?? null)}` : '';
  const target = `${schema && !(engine === 'sqlite' && schema === 'main') ? `${quoteIdent(engine, schema)}.` : ''}${quoteIdent(engine, table)}`;
  if (engine === 'mssql') return `SELECT TOP 100 * FROM ${target}${where};`;
  return `SELECT * FROM ${target}${where} LIMIT 100;`;
}
