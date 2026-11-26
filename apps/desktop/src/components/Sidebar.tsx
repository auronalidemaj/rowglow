import clsx from 'clsx';
import { isFileEngine } from '../lib/types';
import {
  Box,
  ChevronDown,
  ChevronRight,
  Code2,
  Copy,
  Database,
  Eye,
  FunctionSquare,
  Hash,
  History,
  Layers,
  ListTree,
  Play,
  RefreshCw,
  Search,
  Shapes,
  Star,
  Table2,
  TerminalSquare,
  Trash2,
  Eraser,
  Columns3,
  Download,
  FileUp,
  Network,
} from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';
import { qualified, switchesDatabases } from '../lib/dialect';
import { compactCount, relativeTime } from '../lib/format';
import { copyText, mod } from '../lib/platform';
import { call, errorMessage } from '../lib/rpc';
import type { DbObject, HistoryEntry, ObjectKind, StatementOutcome } from '../lib/types';
import { useStore, type Workspace } from '../state/store';
import { quickSelect } from './TableView';
import { IconButton, Segmented, Spinner, useMenu, type MenuItem } from './ui';

const GROUPS: { kind: ObjectKind[]; label: string; icon: typeof Table2 }[] = [
  { kind: ['table', 'foreign'], label: 'Tables', icon: Table2 },
  { kind: ['view'], label: 'Views', icon: Eye },
  { kind: ['matview'], label: 'Materialized views', icon: Layers },
  { kind: ['function', 'procedure'], label: 'Functions', icon: FunctionSquare },
  { kind: ['sequence'], label: 'Sequences', icon: Hash },
  { kind: ['type'], label: 'Types', icon: Shapes },
];

const SYSTEM_SCHEMAS = new Set(['pg_catalog', 'information_schema', 'mysql', 'performance_schema', 'sys', 'pg_toast']);

export function Sidebar({ ws, width }: { ws: Workspace; width: number }) {
  const { setSchema, switchDatabase, loadObjects, updateWorkspace } = useStore.getState();
  const showSystem = useStore((s) => s.settings.showSystemSchemas);
  const engine = ws.connection.engine;

  const schemas = ws.schemas.filter((s) => showSystem || !SYSTEM_SCHEMAS.has(s) || s === ws.schema);
  const dbList = switchesDatabases(engine) ? ws.databases : [];

  return (
    <aside className="sidebar" style={{ width }}>
      <div className="sidebar-selectors">
        {switchesDatabases(engine) && dbList.length > 0 && (
          <label className="selector" title="Database">
            <Database size={13} />
            <select value={ws.info.database} onChange={(e) => switchDatabase(ws.sessionId, e.target.value)}>
              {dbList.map((d) => (
                <option key={d} value={d}>
                  {d}
                </option>
              ))}
            </select>
            <ChevronDown size={12} className="selector-caret" />
          </label>
        )}
        {!isFileEngine(engine) || schemas.length > 1 ? (
          <label className="selector" title={engine === 'mysql' ? 'Database' : 'Schema'}>
            {engine === 'mysql' ? <Database size={13} /> : <ListTree size={13} />}
            <select value={ws.schema} onChange={(e) => setSchema(ws.sessionId, e.target.value)}>
              {schemas.map((s) => (
                <option key={s} value={s}>
                  {s}
                </option>
              ))}
            </select>
            <ChevronDown size={12} className="selector-caret" />
          </label>
        ) : null}
      </div>
      <div className="sidebar-mode">
        <Segmented
          size="sm"
          value={ws.sidebarMode}
          onChange={(m) => updateWorkspace(ws.sessionId, { sidebarMode: m })}
          options={[
            { value: 'items', label: <ListTree size={13} />, title: 'Items' },
            { value: 'queries', label: <Star size={13} />, title: 'Saved queries' },
            { value: 'history', label: <History size={13} />, title: 'History' },
          ]}
        />
        {!ws.info.readOnly && (
          <IconButton label="Import CSV…" onClick={() => useStore.getState().openModal({ kind: 'import', sessionId: ws.sessionId })}>
            <FileUp size={13} />
          </IconButton>
        )}
        <IconButton label="ER diagram" onClick={() => useStore.getState().openDiagram(ws.sessionId)}>
          <Network size={13} />
        </IconButton>
        <IconButton label="Refresh" onClick={() => loadObjects(ws.sessionId)}>
          <RefreshCw size={13} className={clsx(ws.objectsLoading && 'spin')} />
        </IconButton>
      </div>
      {ws.sidebarMode === 'items' && <ObjectTree ws={ws} />}
      {ws.sidebarMode === 'queries' && <SnippetList ws={ws} />}
      {ws.sidebarMode === 'history' && <HistoryList ws={ws} />}
    </aside>
  );
}

function ObjectTree({ ws }: { ws: Workspace }) {
  const [filter, setFilter] = useState('');
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});
  const [menu, openMenu] = useMenu();
  const { openTable, openObject, openQuery, toast, confirm, loadObjects, bumpRefresh, openModal } = useStore.getState();
  const activeTab = ws.tabs.find((t) => t.id === ws.activeTabId);
  const engine = ws.connection.engine;

  const filtered = useMemo(() => {
    const f = filter.trim().toLowerCase();
    return f ? ws.objects.filter((o) => o.name.toLowerCase().includes(f)) : ws.objects;
  }, [ws.objects, filter]);

  const open = (o: DbObject, preview: boolean) => {
    if (['table', 'view', 'matview', 'foreign'].includes(o.kind)) openTable(ws.sessionId, ws.schema, o, { preview });
    else openObject(ws.sessionId, ws.schema, o);
  };

  const runDanger = async (title: string, sql: string, name: string) => {
    const ok = await confirm({
      title,
      message: `Type the table name to confirm. This can’t be undone.`,
      code: sql,
      confirmLabel: title.split(' ')[0],
      danger: true,
      requireText: name,
    });
    if (!ok) return;
    try {
      const [out] = await call<StatementOutcome[]>('query.execute', { sessionId: ws.sessionId, statements: [sql], recordHistory: true });
      if (out.error) throw new Error(out.error.message);
      toast('success', 'Done', sql);
      await loadObjects(ws.sessionId);
      bumpRefresh(ws.sessionId);
    } catch (e) {
      toast('error', 'Failed', errorMessage(e));
    }
  };

  const contextFor = (o: DbObject): MenuItem[] => {
    const isRel = ['table', 'view', 'matview', 'foreign'].includes(o.kind);
    const q = qualified(engine, ws.schema, o.name);
    const items: MenuItem[] = [];
    if (isRel) {
      items.push(
        { label: 'Open data', icon: <Table2 size={14} />, onSelect: () => openTable(ws.sessionId, ws.schema, o) },
        { label: 'Open structure', icon: <Columns3 size={14} />, onSelect: () => openTable(ws.sessionId, ws.schema, o, { view: 'structure' }) },
        { label: 'Open definition', icon: <Code2 size={14} />, onSelect: () => openTable(ws.sessionId, ws.schema, o, { view: 'ddl' }) },
        { separator: true },
        { label: 'New query: SELECT *', icon: <TerminalSquare size={14} />, onSelect: () => openQuery(ws.sessionId, { sql: quickSelect(engine, ws.schema, o.name), title: o.name }) },
        { label: 'Copy name', icon: <Copy size={14} />, onSelect: () => copyText(o.name) },
        { label: 'Copy qualified name', onSelect: () => copyText(q) },
        {
          label: 'Export…',
          icon: <Download size={14} />,
          onSelect: () => openTable(ws.sessionId, ws.schema, o),
        },
        ...(o.kind === 'table' && !ws.info.readOnly
          ? [{ label: 'Import CSV…', icon: <FileUp size={14} />, onSelect: () => openModal({ kind: 'import', sessionId: ws.sessionId, table: o.name }) }]
          : []),
      );
      if (o.kind === 'table' && !ws.info.readOnly) {
        items.push(
          { separator: true },
          {
            label: 'Truncate…',
            icon: <Eraser size={14} />,
            danger: true,
            onSelect: () => runDanger('Truncate table', engine === 'sqlite' ? `DELETE FROM ${q}` : `TRUNCATE TABLE ${q}`, o.name),
          },
          { label: 'Drop…', icon: <Trash2 size={14} />, danger: true, onSelect: () => runDanger('Drop table', `DROP TABLE ${q}`, o.name) },
        );
      }
      if ((o.kind === 'view' || o.kind === 'matview') && !ws.info.readOnly) {
        if (o.kind === 'matview')
          items.push({
            label: 'Refresh materialized view',
            icon: <RefreshCw size={14} />,
            onSelect: () => openQuery(ws.sessionId, { sql: `REFRESH MATERIALIZED VIEW ${q};`, autoRun: true, title: `refresh ${o.name}` }),
          });
        items.push(
          { separator: true },
          {
            label: 'Drop…',
            icon: <Trash2 size={14} />,
            danger: true,
            onSelect: () => runDanger(o.kind === 'view' ? 'Drop view' : 'Drop materialized view', `DROP ${o.kind === 'view' ? 'VIEW' : 'MATERIALIZED VIEW'} ${q}`, o.name),
          },
        );
      }
    } else {
      items.push(
        { label: 'Open definition', icon: <Code2 size={14} />, onSelect: () => openObject(ws.sessionId, ws.schema, o) },
        { label: 'Copy name', icon: <Copy size={14} />, onSelect: () => copyText(o.name) },
      );
    }
    void openModal;
    return items;
  };

  return (
    <div className="tree">
      <div className="tree-search">
        <Search size={13} />
        <input
          className="tree-search-input"
          placeholder={`Search ${ws.objects.length} items`}
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          spellCheck={false}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && filtered[0]) open(filtered[0], false);
            if (e.key === 'Escape') setFilter('');
          }}
        />
      </div>
      <div className="tree-scroll">
        {ws.objectsError && <div className="tree-error">{ws.objectsError}</div>}
        {ws.objectsLoading && !ws.objects.length && (
          <div className="tree-loading">
            <Spinner />
          </div>
        )}
        {!ws.objectsLoading && !ws.objects.length && !ws.objectsError && (
          <div className="tree-empty">
            <Box size={18} />
            <div>No tables in <b>{ws.schema}</b> yet.</div>
            <button className="link-btn" onClick={() => openQuery(ws.sessionId, { sql: `CREATE TABLE example (\n  id ${engine === 'postgres' ? 'bigint GENERATED ALWAYS AS IDENTITY' : engine === 'mysql' ? 'BIGINT AUTO_INCREMENT' : engine === 'mssql' ? 'INT IDENTITY(1,1)' : 'INTEGER'} PRIMARY KEY,\n  name ${engine === 'mssql' ? 'NVARCHAR(200)' : 'TEXT'} NOT NULL\n);` })}>
              Create one in the editor
            </button>
          </div>
        )}
        {GROUPS.map((g) => {
          const items = filtered.filter((o) => g.kind.includes(o.kind));
          if (!items.length) return null;
          const key = g.label;
          const isCollapsed = collapsed[key] && !filter;
          const Icon = g.icon;
          return (
            <div key={key} className="tree-group">
              <button className="tree-group-head" onClick={() => setCollapsed((c) => ({ ...c, [key]: !c[key] }))}>
                {isCollapsed ? <ChevronRight size={12} /> : <ChevronDown size={12} />}
                <span>{g.label}</span>
                <span className="tree-group-count">{items.length}</span>
              </button>
              {!isCollapsed &&
                items.map((o) => {
                  const isActive =
                    activeTab &&
                    ((activeTab.kind === 'table' && activeTab.name === o.name && activeTab.schema === ws.schema) ||
                      (activeTab.kind === 'object' && activeTab.name === o.name && activeTab.signature === o.signature));
                  return (
                    <button
                      key={`${o.kind}:${o.name}:${o.signature ?? ''}`}
                      className={clsx('tree-item', isActive && 'is-active')}
                      onClick={() => open(o, true)}
                      onDoubleClick={() => open(o, false)}
                      onContextMenu={(e) => openMenu(e, contextFor(o))}
                      title={o.comment ? `${o.name} — ${o.comment}` : o.signature !== undefined ? `${o.name}(${o.signature})` : o.name}
                    >
                      <Icon size={13} className={clsx('tree-icon', `kind-${o.kind}`)} />
                      <span className="tree-label">
                        {o.name}
                        {o.signature !== undefined && o.signature !== null && <span className="tree-sig">({o.signature})</span>}
                      </span>
                      {o.rowsEstimate !== undefined && o.rowsEstimate !== null && o.rowsEstimate > 0 && (
                        <span className="tree-meta">{compactCount(o.rowsEstimate)}</span>
                      )}
                    </button>
                  );
                })}
            </div>
          );
        })}
      </div>
      {menu}
    </div>
  );
}

function SnippetList({ ws }: { ws: Workspace }) {
  const snippets = useStore((s) => s.snippets);
  const { openQuery, loadSnippets, confirm, toast } = useStore.getState();
  const [menu, openMenu] = useMenu();
  const [filter, setFilter] = useState('');
  useEffect(() => {
    void loadSnippets();
  }, [loadSnippets]);
  const list = snippets.filter(
    (s) => (!s.connectionId || s.connectionId === ws.connection.id) && s.name.toLowerCase().includes(filter.toLowerCase()),
  );
  return (
    <div className="tree">
      <div className="tree-search">
        <Search size={13} />
        <input className="tree-search-input" placeholder="Search saved queries" value={filter} onChange={(e) => setFilter(e.target.value)} />
      </div>
      <div className="tree-scroll">
        {!list.length && (
          <div className="tree-empty">
            <Star size={18} />
            <div>Save queries from the editor with {mod}S.</div>
          </div>
        )}
        {list.map((s) => (
          <button
            key={s.id}
            className="tree-item tall"
            onClick={() => openQuery(ws.sessionId, { sql: s.sql, title: s.name, snippetId: s.id })}
            onContextMenu={(e) =>
              openMenu(e, [
                { label: 'Open', onSelect: () => openQuery(ws.sessionId, { sql: s.sql, title: s.name, snippetId: s.id }) },
                { label: 'Run', icon: <Play size={14} />, onSelect: () => openQuery(ws.sessionId, { sql: s.sql, title: s.name, autoRun: true }) },
                { label: 'Copy SQL', icon: <Copy size={14} />, onSelect: () => copyText(s.sql) },
                { separator: true },
                {
                  label: 'Delete',
                  icon: <Trash2 size={14} />,
                  danger: true,
                  onSelect: async () => {
                    if (await confirm({ title: `Delete “${s.name}”?`, confirmLabel: 'Delete', danger: true })) {
                      await call('snippets.delete', { id: s.id });
                      await loadSnippets();
                      toast('success', 'Query deleted');
                    }
                  },
                },
              ])
            }
          >
            <Star size={13} className="tree-icon kind-snippet" />
            <span className="tree-label">
              {s.name}
              <span className="tree-sub mono">{s.sql.replace(/\s+/g, ' ').slice(0, 80)}</span>
            </span>
          </button>
        ))}
      </div>
      {menu}
    </div>
  );
}

function HistoryList({ ws }: { ws: Workspace }) {
  const [items, setItems] = useState<HistoryEntry[] | null>(null);
  const [filter, setFilter] = useState('');
  const { openQuery, confirm } = useStore.getState();
  const license = useStore((s) => s.license);
  const [menu, openMenu] = useMenu();
  useEffect(() => {
    const t = setTimeout(() => {
      call<HistoryEntry[]>('history.list', { connectionId: ws.connection.id, search: filter || null, limit: 300 })
        .then(setItems)
        .catch(() => setItems([]));
    }, 150);
    return () => clearTimeout(t);
  }, [ws.connection.id, filter, ws.refreshKey]);
  return (
    <div className="tree">
      <div className="tree-search">
        <Search size={13} />
        <input className="tree-search-input" placeholder="Search history" value={filter} onChange={(e) => setFilter(e.target.value)} />
        <IconButton
          label="Clear history"
          onClick={async () => {
            if (await confirm({ title: 'Clear query history for this connection?', confirmLabel: 'Clear', danger: true })) {
              await call('history.clear', { connectionId: ws.connection.id });
              setItems([]);
            }
          }}
        >
          <Trash2 size={12} />
        </IconButton>
      </div>
      <div className="tree-scroll">
        {items === null && (
          <div className="tree-loading">
            <Spinner />
          </div>
        )}
        {items?.length === 0 && (
          <div className="tree-empty">
            <History size={18} />
            <div>Queries you run will show up here.</div>
          </div>
        )}
        {items?.map((h) => (
          <button
            key={h.id}
            className={clsx('tree-item tall history-item', h.error && 'is-error')}
            onClick={() => openQuery(ws.sessionId, { sql: h.sql.endsWith(';') ? h.sql : h.sql + ';', title: 'History' })}
            onContextMenu={(e) =>
              openMenu(e, [
                { label: 'Open in new tab', onSelect: () => openQuery(ws.sessionId, { sql: h.sql, title: 'History' }) },
                { label: 'Run again', icon: <Play size={14} />, onSelect: () => openQuery(ws.sessionId, { sql: h.sql, title: 'History', autoRun: true }) },
                { label: 'Copy SQL', icon: <Copy size={14} />, onSelect: () => copyText(h.sql) },
              ])
            }
            title={h.error ?? h.sql}
          >
            <span className="tree-label">
              <span className="mono history-sql">{h.sql.replace(/\s+/g, ' ').slice(0, 140)}</span>
              <span className="tree-sub">
                {relativeTime(h.createdAt)} · {h.error ? 'error' : h.rows !== null ? `${h.rows} rows` : 'ok'} · {Math.round(h.durationMs)} ms
              </span>
            </span>
          </button>
        ))}
        {license && license.limits.history != null && items && items.length >= license.limits.history && (
          <div className="tree-note">Free plan keeps the last {license.limits.history} queries.</div>
        )}
      </div>
      {menu}
    </div>
  );
}
