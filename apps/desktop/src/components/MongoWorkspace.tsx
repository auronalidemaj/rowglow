import clsx from 'clsx';
import {
  Braces,
  ChevronLeft,
  ChevronRight,
  Copy,
  Database,
  Eye,
  FileJson,
  Layers,
  ListTree,
  Pencil,
  Play,
  Plus,
  RefreshCw,
  Search,
  Table2,
  TerminalSquare,
  Trash2,
} from 'lucide-react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { compactCount } from '../lib/format';
import { ejsonCell, shellToJson } from '../lib/mongoShell';
import { copyText } from '../lib/platform';
import { call, errorMessage, RpcError } from '../lib/rpc';
import { useStore, type Workspace } from '../state/store';
import { Button, EmptyState, IconButton, Modal, Segmented, Spinner } from './ui';

type Doc = Record<string, unknown>;

interface CollectionInfo {
  name: string;
  kind: string;
  count: number | null;
}

const pretty = (v: unknown) => JSON.stringify(v, null, 2);

/** Converts what the user typed (mongosh syntax) to Extended JSON, or throws. */
function toJson(text: string): string {
  return shellToJson(text);
}

export function MongoWorkspace({ ws, visible }: { ws: Workspace; visible: boolean }) {
  const [db, setDb] = useState(ws.info.database);
  const [dbs, setDbs] = useState<string[]>([ws.info.database]);
  const [collections, setCollections] = useState<CollectionInfo[] | null>(null);
  const [collError, setCollError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [mode, setMode] = useState<'docs' | 'aggregate' | 'indexes' | 'command'>('docs');
  const [filter, setFilter] = useState('');
  const [creating, setCreating] = useState(false);
  const { toast, confirm } = useStore.getState();
  const readOnly = ws.info.readOnly;

  useEffect(() => {
    call<string[]>('mongo.databases', { sessionId: ws.sessionId })
      .then(setDbs)
      .catch(() => {});
  }, [ws.sessionId]);

  const loadCollections = useCallback(async () => {
    setCollError(null);
    try {
      setCollections(await call<CollectionInfo[]>('mongo.collections', { sessionId: ws.sessionId, db }));
    } catch (e) {
      setCollections([]);
      setCollError(errorMessage(e));
    }
  }, [ws.sessionId, db]);

  useEffect(() => {
    setCollections(null);
    setSelected(null);
    void loadCollections();
  }, [loadCollections]);

  const shown = (collections ?? []).filter((c) => !filter || c.name.toLowerCase().includes(filter.toLowerCase()));

  const drop = async (name: string) => {
    const ok = await confirm({
      title: `Drop collection ${name}?`,
      message: `Every document and index in ${db}.${name} is deleted. Type the collection name to confirm. This can’t be undone.`,
      confirmLabel: 'Drop',
      danger: true,
      requireText: name,
    });
    if (!ok) return;
    try {
      await call('mongo.dropCollection', { sessionId: ws.sessionId, db, name });
      toast('success', 'Collection dropped', name);
      if (selected === name) setSelected(null);
      await loadCollections();
    } catch (e) {
      toast('error', 'Couldn’t drop the collection', errorMessage(e));
    }
  };

  return (
    <div className="workspace redis-ws" style={{ display: visible ? 'flex' : 'none' }}>
      <aside className="sidebar redis-sidebar">
        <div className="sidebar-selectors">
          <label className="selector" title="Database">
            <Database size={13} />
            <select value={db} onChange={(e) => setDb(e.target.value)}>
              {dbs.map((d) => (
                <option key={d} value={d}>
                  {d}
                </option>
              ))}
            </select>
          </label>
        </div>
        <div className="tree-search">
          <Search size={13} />
          <input className="tree-search-input" value={filter} onChange={(e) => setFilter(e.target.value)} placeholder={`Search ${collections?.length ?? 0} collections`} />
        </div>
        <div className="redis-listbar">
          <span className="dim">Collections</span>
          <div className="toolbar-spacer" />
          {!readOnly && (
            <IconButton label="New collection" onClick={() => setCreating(true)}>
              <Plus size={14} />
            </IconButton>
          )}
          <IconButton label="Refresh" onClick={() => void loadCollections()}>
            <RefreshCw size={13} />
          </IconButton>
        </div>
        <div className="redis-keys">
          {collections === null && (
            <div className="tree-empty">
              <Spinner />
            </div>
          )}
          {collError && <div className="tree-error">{collError}</div>}
          {shown.map((c) => (
            <button
              key={c.name}
              className={clsx('redis-key mongo-coll', selected === c.name && 'is-active')}
              onClick={() => {
                setSelected(c.name);
                if (mode === 'command') setMode('docs');
              }}
              onContextMenu={(e) => {
                e.preventDefault();
                if (!readOnly) void drop(c.name);
              }}
              title={readOnly ? c.name : `${c.name} — right-click to drop`}
            >
              {c.kind === 'view' ? <Eye size={13} className="dim" /> : <Layers size={13} className="dim" />}
              <span className="redis-key-name mono">{c.name}</span>
              {c.count !== null && <span className="tree-count">{compactCount(c.count)}</span>}
            </button>
          ))}
          {collections && !collections.length && !collError && <div className="tree-empty">No collections in {db} yet.</div>}
        </div>
      </aside>
      <main className="main redis-main">
        <div className="redis-modebar">
          <Segmented
            size="sm"
            value={mode}
            onChange={setMode}
            options={[
              { value: 'docs', label: <><FileJson size={13} /> Documents</> },
              { value: 'aggregate', label: <><ListTree size={13} /> Aggregate</> },
              { value: 'indexes', label: <><Braces size={13} /> Indexes</> },
              { value: 'command', label: <><TerminalSquare size={13} /> Command</> },
            ]}
          />
          {selected && mode !== 'command' && (
            <span className="mono mongo-target">
              <span className="dim">{db}.</span>
              {selected}
            </span>
          )}
          <div className="toolbar-spacer" />
          {readOnly && <span className="tag">read-only</span>}
        </div>
        {mode === 'command' ? (
          <CommandView ws={ws} db={db} onWrite={() => void loadCollections()} />
        ) : !selected ? (
          <EmptyState icon={<Layers size={22} />} title="Pick a collection">
            Or use Command to run any database command, like <span className="mono">{'{ dbStats: 1 }'}</span>.
          </EmptyState>
        ) : mode === 'docs' ? (
          <DocumentsView key={`${db}.${selected}`} ws={ws} db={db} coll={selected} onChanged={() => void loadCollections()} />
        ) : mode === 'aggregate' ? (
          <AggregateView key={`${db}.${selected}`} ws={ws} db={db} coll={selected} />
        ) : (
          <IndexesView key={`${db}.${selected}`} ws={ws} db={db} coll={selected} />
        )}
      </main>
      {creating && (
        <NameModal
          title="New collection"
          label={`Collection in ${db}`}
          onClose={() => setCreating(false)}
          onSubmit={async (name) => {
            try {
              await call('mongo.createCollection', { sessionId: ws.sessionId, db, name });
              setCreating(false);
              await loadCollections();
              setSelected(name);
            } catch (e) {
              toast('error', 'Couldn’t create the collection', errorMessage(e));
            }
          }}
        />
      )}
    </div>
  );
}

// ---- documents ------------------------------------------------------------------

function DocumentsView({ ws, db, coll, onChanged }: { ws: Workspace; db: string; coll: string; onChanged: () => void }) {
  const { toast, confirm } = useStore.getState();
  const [filter, setFilter] = useState('');
  const [sort, setSort] = useState('');
  const [projection, setProjection] = useState('');
  const [more, setMore] = useState(false);
  const [limit, setLimit] = useState(50);
  const [skip, setSkip] = useState(0);
  const [view, setView] = useState<'table' | 'json'>('table');
  const [res, setRes] = useState<{ docs: Doc[]; total: number | null; durationMs: number } | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<{ doc: Doc | null } | null>(null);
  const readOnly = ws.info.readOnly;
  const applied = useRef({ filter: '', sort: '', projection: '' });

  const run = useCallback(
    async (opts: { skip?: number; fresh?: boolean } = {}) => {
      setLoading(true);
      setError(null);
      try {
        if (opts.fresh) applied.current = { filter: toJson(filter), sort: toJson(sort), projection: toJson(projection) };
        const r = await call<{ docs: Doc[]; total: number | null; durationMs: number }>('mongo.find', {
          sessionId: ws.sessionId,
          db,
          collection: coll,
          ...applied.current,
          skip: opts.skip ?? skip,
          limit,
        });
        setRes(r);
        if (opts.skip !== undefined) setSkip(opts.skip);
      } catch (e) {
        setError(errorMessage(e));
      } finally {
        setLoading(false);
      }
    },
    [ws.sessionId, db, coll, filter, sort, projection, skip, limit],
  );

  useEffect(() => {
    void run({ skip: 0, fresh: true });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [db, coll, limit]);

  const columns = useMemo(() => {
    const cols: string[] = [];
    for (const d of res?.docs.slice(0, 100) ?? []) for (const k of Object.keys(d)) if (!cols.includes(k)) cols.push(k);
    return cols.sort((a, b) => (a === '_id' ? -1 : b === '_id' ? 1 : 0));
  }, [res]);

  const del = async (doc: Doc) => {
    const ok = await confirm({
      title: 'Delete this document?',
      message: `It is removed from ${db}.${coll}. This can’t be undone.`,
      code: pretty(doc._id),
      confirmLabel: 'Delete',
      danger: true,
    });
    if (!ok) return;
    try {
      const r = await call<{ deleted: number }>('mongo.delete', { sessionId: ws.sessionId, db, collection: coll, ids: [JSON.stringify(doc._id)] });
      toast(r.deleted ? 'success' : 'info', r.deleted ? 'Document deleted' : 'It was already gone');
      await run();
      onChanged();
    } catch (e) {
      toast('error', 'Couldn’t delete', errorMessage(e));
    }
  };

  const total = res?.total ?? null;
  const from = res && res.docs.length ? skip + 1 : 0;
  const to = skip + (res?.docs.length ?? 0);

  return (
    <div className="mongo-docs">
      <form
        className="mongo-query"
        onSubmit={(e) => {
          e.preventDefault();
          void run({ skip: 0, fresh: true });
        }}
      >
        <div className="mongo-qrow">
          <span className="mongo-qlabel">Filter</span>
          <input className="input input-sm mono" value={filter} onChange={(e) => setFilter(e.target.value)} placeholder='{ status: "active", age: { $gt: 30 } }' spellCheck={false} />
          <Button size="sm" variant="primary" type="submit" disabled={loading}>
            {loading ? <Spinner size={11} /> : <Play size={12} />} Find
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setMore((m) => !m)}>
            {more ? 'Less' : 'Sort / fields'}
          </Button>
        </div>
        {more && (
          <div className="mongo-qrow">
            <span className="mongo-qlabel">Sort</span>
            <input className="input input-sm mono" value={sort} onChange={(e) => setSort(e.target.value)} placeholder="{ createdAt: -1 }" spellCheck={false} />
            <span className="mongo-qlabel">Fields</span>
            <input className="input input-sm mono" value={projection} onChange={(e) => setProjection(e.target.value)} placeholder="{ name: 1, email: 1 }" spellCheck={false} />
          </div>
        )}
      </form>
      <div className="mongo-resultbar">
        <Segmented
          size="sm"
          value={view}
          onChange={setView}
          options={[
            { value: 'table', label: <><Table2 size={12} /> Table</> },
            { value: 'json', label: <><Braces size={12} /> JSON</> },
          ]}
        />
        {!readOnly && (
          <Button size="sm" onClick={() => setEditing({ doc: null })}>
            <Plus size={12} /> Insert
          </Button>
        )}
        <div className="toolbar-spacer" />
        <span className="dim">
          {res ? `${from.toLocaleString()}–${to.toLocaleString()}${total !== null ? ` of ${total.toLocaleString()}` : ''} · ${Math.round(res.durationMs)} ms` : ''}
        </span>
        <IconButton label="Previous page" disabled={skip === 0 || loading} onClick={() => void run({ skip: Math.max(0, skip - limit) })}>
          <ChevronLeft size={14} />
        </IconButton>
        <IconButton label="Next page" disabled={loading || !res || res.docs.length < limit || (total !== null && to >= total)} onClick={() => void run({ skip: skip + limit })}>
          <ChevronRight size={14} />
        </IconButton>
        <select className="select select-sm" value={limit} onChange={(e) => setLimit(Number(e.target.value))} title="Page size">
          {[20, 50, 100, 500].map((n) => (
            <option key={n} value={n}>
              {n} / page
            </option>
          ))}
        </select>
      </div>
      {error && <div className="callout callout-error mongo-error">{error}</div>}
      <div className="mongo-results">
        {res && !res.docs.length && <EmptyState title="No documents">{applied.current.filter !== '{}' ? 'Nothing matches this filter.' : 'This collection is empty.'}</EmptyState>}
        {res && res.docs.length > 0 && view === 'table' && (
          <table className="rk-table mongo-table">
            <thead>
              <tr>
                {columns.map((c) => (
                  <th key={c}>{c}</th>
                ))}
                <th className="rk-row-actions" />
              </tr>
            </thead>
            <tbody>
              {res.docs.map((d, i) => (
                <tr key={i} onDoubleClick={() => setEditing({ doc: d })}>
                  {columns.map((c) => {
                    if (!(c in d)) return <td key={c} className="mongo-missing" />;
                    const cell = ejsonCell(d[c]);
                    return (
                      <td key={c} className={clsx('mono', `mc-${cell.kind}`)} title={cell.kind === 'obj' || cell.kind === 'arr' ? pretty(d[c]) : cell.text}>
                        {cell.text}
                      </td>
                    );
                  })}
                  <td className="rk-row-actions">
                    <IconButton label={readOnly ? 'View' : 'Edit'} onClick={() => setEditing({ doc: d })}>
                      {readOnly ? <Eye size={12} /> : <Pencil size={12} />}
                    </IconButton>
                    {!readOnly && (
                      <IconButton label="Delete" className="danger-hover" onClick={() => void del(d)}>
                        <Trash2 size={12} />
                      </IconButton>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {res && res.docs.length > 0 && view === 'json' && (
          <div className="mongo-json-list">
            {res.docs.map((d, i) => (
              <div key={i} className="mongo-json-card">
                <div className="mongo-json-actions">
                  <IconButton label="Copy JSON" onClick={() => copyText(pretty(d)).then(() => toast('success', 'Copied'))}>
                    <Copy size={12} />
                  </IconButton>
                  <IconButton label={readOnly ? 'View' : 'Edit'} onClick={() => setEditing({ doc: d })}>
                    {readOnly ? <Eye size={12} /> : <Pencil size={12} />}
                  </IconButton>
                  {!readOnly && (
                    <IconButton label="Delete" className="danger-hover" onClick={() => void del(d)}>
                      <Trash2 size={12} />
                    </IconButton>
                  )}
                </div>
                <pre className="mono">{pretty(d)}</pre>
              </div>
            ))}
          </div>
        )}
      </div>
      {editing && (
        <DocEditor
          title={editing.doc ? (readOnly ? 'Document' : 'Edit document') : `Insert into ${coll}`}
          initial={editing.doc ? pretty(editing.doc) : '{\n  \n}'}
          readOnly={readOnly}
          hint={
            editing.doc
              ? 'Extended JSON: keep {"$oid": …}, {"$date": …} wrappers to preserve types. The _id can’t change.'
              : 'mongosh syntax works: { name: "Zoë", at: new Date(), tags: ["a"] }. Use [ … ] to insert several documents.'
          }
          onClose={() => setEditing(null)}
          onSave={async (text) => {
            const json = toJson(text);
            if (editing.doc) {
              await call('mongo.replace', { sessionId: ws.sessionId, db, collection: coll, id: JSON.stringify(editing.doc._id), doc: json });
              toast('success', 'Document saved');
            } else {
              const parsed = JSON.parse(json);
              const docs = (Array.isArray(parsed) ? parsed : [parsed]).map((d: unknown) => JSON.stringify(d));
              const r = await call<{ inserted: number }>('mongo.insert', { sessionId: ws.sessionId, db, collection: coll, docs });
              toast('success', `Inserted ${r.inserted} document${r.inserted === 1 ? '' : 's'}`);
              onChanged();
            }
            setEditing(null);
            await run();
          }}
        />
      )}
    </div>
  );
}

function DocEditor({
  title,
  initial,
  readOnly,
  hint,
  onClose,
  onSave,
}: {
  title: string;
  initial: string;
  readOnly: boolean;
  hint: string;
  onClose: () => void;
  onSave: (text: string) => Promise<void>;
}) {
  const [text, setText] = useState(initial);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      await onSave(text);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title={title}
      onClose={onClose}
      width={720}
      footer={
        <>
          <span className="dim mongo-hint">{readOnly ? 'Read-only connection' : hint}</span>
          <div className="toolbar-spacer" />
          <Button variant="ghost" onClick={onClose}>
            {readOnly ? 'Close' : 'Cancel'}
          </Button>
          {!readOnly && (
            <Button variant="primary" onClick={save} disabled={busy}>
              {busy ? <Spinner size={11} /> : null} Save
            </Button>
          )}
        </>
      }
    >
      <textarea
        className="input mono mongo-editor"
        value={text}
        readOnly={readOnly}
        spellCheck={false}
        autoFocus
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && (e.key === 's' || e.key === 'Enter') && !readOnly) {
            e.preventDefault();
            void save();
          }
        }}
      />
      {error && <div className="callout callout-error mongo-error">{error}</div>}
    </Modal>
  );
}

// ---- aggregate / indexes / command -------------------------------------------------

function AggregateView({ ws, db, coll }: { ws: Workspace; db: string; coll: string }) {
  const [pipeline, setPipeline] = useState('[\n  { $match: {} },\n  { $group: { _id: null, count: { $sum: 1 } } }\n]');
  const [res, setRes] = useState<{ docs: Doc[]; truncated: boolean; durationMs: number } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setBusy(true);
    setError(null);
    try {
      setRes(await call('mongo.aggregate', { sessionId: ws.sessionId, db, collection: coll, pipeline: toJson(pipeline) }));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="mongo-split">
      <div className="mongo-input-pane">
        <textarea
          className="input mono mongo-editor"
          value={pipeline}
          spellCheck={false}
          onChange={(e) => setPipeline(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
              e.preventDefault();
              void run();
            }
          }}
        />
        <div className="rk-actions">
          <span className="dim">Pipeline stages in mongosh syntax · ⌘↵ to run · results are capped at 1,000</span>
          <div className="toolbar-spacer" />
          <Button size="sm" variant="primary" onClick={run} disabled={busy}>
            {busy ? <Spinner size={11} /> : <Play size={12} />} Run
          </Button>
        </div>
      </div>
      <div className="mongo-results">
        {error && <div className="callout callout-error mongo-error">{error}</div>}
        {res && (
          <>
            <div className="dim mongo-count">
              {res.docs.length.toLocaleString()} result{res.docs.length === 1 ? '' : 's'}
              {res.truncated ? ' (first 1,000)' : ''} · {Math.round(res.durationMs)} ms
            </div>
            <div className="mongo-json-list">
              {res.docs.map((d, i) => (
                <div key={i} className="mongo-json-card">
                  <pre className="mono">{pretty(d)}</pre>
                </div>
              ))}
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function IndexesView({ ws, db, coll }: { ws: Workspace; db: string; coll: string }) {
  const [idx, setIdx] = useState<{ name: string; keys: Doc; unique: boolean; sparse: boolean; ttlSeconds: number | null; partial: Doc | null }[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    call<typeof idx>('mongo.indexes', { sessionId: ws.sessionId, db, collection: coll })
      .then(setIdx)
      .catch((e) => setError(errorMessage(e)));
  }, [ws.sessionId, db, coll]);
  if (error) return <EmptyState title="Couldn’t list indexes">{error}</EmptyState>;
  if (!idx) return <Spinner />;
  return (
    <div className="mongo-results">
      <table className="rk-table">
        <thead>
          <tr>
            <th>Name</th>
            <th>Keys</th>
            <th>Options</th>
          </tr>
        </thead>
        <tbody>
          {idx.map((i) => (
            <tr key={i.name}>
              <td className="mono">{i.name}</td>
              <td className="mono">{JSON.stringify(i.keys)}</td>
              <td className="dim">
                {[i.unique && 'unique', i.sparse && 'sparse', i.ttlSeconds !== null && `TTL ${i.ttlSeconds}s`, i.partial && `partial ${JSON.stringify(i.partial)}`]
                  .filter(Boolean)
                  .join(' · ') || '—'}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function CommandView({ ws, db, onWrite }: { ws: Workspace; db: string; onWrite: () => void }) {
  const { confirm } = useStore.getState();
  const [text, setText] = useState('{ dbStats: 1 }');
  const [out, setOut] = useState<{ result: unknown; durationMs: number } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const run = async (confirmed = false) => {
    setBusy(true);
    setError(null);
    try {
      const command = toJson(text);
      const r = await call<{ result: unknown; durationMs: number; write?: boolean }>('mongo.command', { sessionId: ws.sessionId, db, command, confirmed });
      setOut(r);
      if (r.write) onWrite();
    } catch (e) {
      if (e instanceof RpcError && e.code === 'confirm_required' && !confirmed) {
        const ok = await confirm({ title: 'Run this command?', message: e.message, code: text, confirmLabel: 'Run', danger: true });
        if (ok) return run(true);
      } else setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="mongo-split">
      <div className="mongo-input-pane">
        <textarea
          className="input mono mongo-editor mongo-command"
          value={text}
          spellCheck={false}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
              e.preventDefault();
              void run();
            }
          }}
        />
        <div className="rk-actions">
          <span className="dim">Runs against {db} · e.g. {'{ collStats: "users" }'}, {'{ serverStatus: 1 }'} · ⌘↵ to run</span>
          <div className="toolbar-spacer" />
          <Button size="sm" variant="primary" onClick={() => run()} disabled={busy}>
            {busy ? <Spinner size={11} /> : <Play size={12} />} Run
          </Button>
        </div>
      </div>
      <div className="mongo-results">
        {error && <div className="callout callout-error mongo-error">{error}</div>}
        {out && (
          <>
            <div className="dim mongo-count">{Math.round(out.durationMs)} ms</div>
            <div className="mongo-json-card">
              <pre className="mono">{pretty(out.result)}</pre>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function NameModal({ title, label, onClose, onSubmit }: { title: string; label: string; onClose: () => void; onSubmit: (name: string) => Promise<void> }) {
  const [name, setName] = useState('');
  return (
    <Modal
      title={title}
      onClose={onClose}
      width={420}
      footer={
        <>
          <div className="toolbar-spacer" />
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" disabled={!name.trim()} onClick={() => onSubmit(name.trim())}>
            Create
          </Button>
        </>
      }
    >
      <form
        className="conn-form"
        onSubmit={(e) => {
          e.preventDefault();
          if (name.trim()) void onSubmit(name.trim());
        }}
      >
        <label className="field">
          <span className="field-label">{label}</span>
          <input className="input mono" value={name} onChange={(e) => setName(e.target.value)} autoFocus spellCheck={false} />
        </label>
      </form>
    </Modal>
  );
}
