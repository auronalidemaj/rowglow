import { useVirtualizer } from '@tanstack/react-virtual';
import clsx from 'clsx';
import { Check, Clock, Copy, KeyRound, Pencil, Plus, RefreshCw, Search, Server, TerminalSquare, Trash2, X } from 'lucide-react';
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import { compactCount, tryPrettyJson } from '../lib/format';
import { copyText } from '../lib/platform';
import { call, errorMessage, RpcError } from '../lib/rpc';
import { useStore, type Workspace } from '../state/store';
import { Button, EmptyState, Field, IconButton, Modal, Segmented, Spinner } from './ui';

// ---- types --------------------------------------------------------------------

interface Bin {
  text: string;
  b64?: string;
}

interface RedisKey extends Bin {
  type: string;
  ttl: number;
}

interface KeyDetail {
  type: string;
  ttl: number;
  length?: number;
  truncated?: boolean;
  value?: Bin;
  items?: Record<string, unknown>[];
  unsupported?: string;
}

type Reply =
  | { t: 'nil' }
  | { t: 'int'; v: number }
  | { t: 'str'; text: string; b64?: string }
  | { t: 'status'; v: string }
  | { t: 'double'; v: number }
  | { t: 'bool'; v: boolean }
  | { t: 'bignum'; v: string }
  | { t: 'error'; v: string }
  | { t: 'array'; v: Reply[] }
  | { t: 'map'; v: [Reply, Reply][] };

const TYPES = ['string', 'hash', 'list', 'set', 'zset', 'stream'];

function ttlLabel(ms: number): string {
  if (ms < 0) return 'No expiry';
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
  return `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h`;
}

function TypeBadge({ type }: { type: string }) {
  const label = type === 'ReJSON-RL' ? 'json' : type === 'string' ? 'str' : type;
  return <span className={clsx('rk-type', `rk-t-${label}`)}>{label}</span>;
}

/** redis-cli style rendering of a reply. */
function renderReply(r: Reply, indent = ''): string {
  switch (r.t) {
    case 'nil':
      return '(nil)';
    case 'int':
      return `(integer) ${r.v}`;
    case 'double':
      return `(double) ${r.v}`;
    case 'bool':
      return `(boolean) ${r.v}`;
    case 'bignum':
      return `(big number) ${r.v}`;
    case 'status':
      return r.v;
    case 'error':
      return `(error) ${r.v}`;
    case 'str':
      return JSON.stringify(r.text);
    case 'array': {
      if (!r.v.length) return '(empty array)';
      const w = String(r.v.length).length;
      return r.v
        .map((x, i) => {
          const prefix = `${String(i + 1).padStart(w)}) `;
          return (i ? indent : '') + prefix + renderReply(x, indent + ' '.repeat(prefix.length));
        })
        .join('\n');
    }
    case 'map': {
      if (!r.v.length) return '(empty map)';
      return r.v.map(([k, v], i) => `${i ? indent : ''}${i + 1}# ${renderReply(k)} => ${renderReply(v, indent + '   ')}`).join('\n');
    }
  }
}

// ---- workspace ------------------------------------------------------------------

export function RedisWorkspace({ ws, visible }: { ws: Workspace; visible: boolean }) {
  const [mode, setMode] = useState<'browser' | 'console' | 'server'>('browser');
  const [selected, setSelected] = useState<RedisKey | null>(null);
  const [reloadKey, setReloadKey] = useState(0);
  const [newKey, setNewKey] = useState(false);
  const readOnly = ws.info.readOnly;

  return (
    <div className="workspace redis-ws" style={{ display: visible ? 'flex' : 'none' }}>
      <KeyBrowser
        ws={ws}
        selected={selected}
        onSelect={(k) => {
          setSelected(k);
          setMode('browser');
        }}
        reloadKey={reloadKey}
        onNewKey={readOnly ? undefined : () => setNewKey(true)}
      />
      <main className="main redis-main">
        <div className="redis-modebar">
          <Segmented
            size="sm"
            value={mode}
            onChange={setMode}
            options={[
              {
                value: 'browser',
                label: (
                  <>
                    <KeyRound size={13} /> Key
                  </>
                ),
              },
              {
                value: 'console',
                label: (
                  <>
                    <TerminalSquare size={13} /> Console
                  </>
                ),
              },
              {
                value: 'server',
                label: (
                  <>
                    <Server size={13} /> Server
                  </>
                ),
              },
            ]}
          />
          {readOnly && <span className="tag">read-only</span>}
        </div>
        <div className="redis-pane" style={{ display: mode === 'browser' ? 'flex' : 'none' }}>
          {selected ? (
            <KeyView
              ws={ws}
              k={selected}
              onDeleted={() => {
                setSelected(null);
                setReloadKey((n) => n + 1);
              }}
              onRenamed={(name) => {
                setSelected({ ...selected, text: name, b64: undefined });
                setReloadKey((n) => n + 1);
              }}
            />
          ) : (
            <EmptyState icon={<KeyRound size={22} />} title="Pick a key">
              Search with a pattern like <span className="mono">user:*</span>, or open the console to run commands.
            </EmptyState>
          )}
        </div>
        <div className="redis-pane" style={{ display: mode === 'console' ? 'flex' : 'none' }}>
          <RedisConsole ws={ws} active={visible && mode === 'console'} onWrite={() => setReloadKey((n) => n + 1)} />
        </div>
        <div className="redis-pane" style={{ display: mode === 'server' ? 'flex' : 'none' }}>
          {mode === 'server' && <ServerInfo ws={ws} />}
        </div>
      </main>
      {newKey && (
        <NewKeyModal
          ws={ws}
          onClose={() => setNewKey(false)}
          onCreated={(key, type) => {
            setNewKey(false);
            setReloadKey((n) => n + 1);
            setSelected({ text: key, type, ttl: -1 });
          }}
        />
      )}
    </div>
  );
}

// ---- key list -------------------------------------------------------------------

function KeyBrowser({
  ws,
  selected,
  onSelect,
  reloadKey,
  onNewKey,
}: {
  ws: Workspace;
  selected: RedisKey | null;
  onSelect: (k: RedisKey) => void;
  reloadKey: number;
  onNewKey?: () => void;
}) {
  const { switchDatabase } = useStore.getState();
  const [pattern, setPattern] = useState('*');
  const [applied, setApplied] = useState('*');
  const [kind, setKind] = useState('');
  const [keys, setKeys] = useState<RedisKey[]>([]);
  const [cursor, setCursor] = useState('0');
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [overview, setOverview] = useState<{
    keyspace: { db: number; keys: number }[];
    dbsize: number;
  } | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const gen = useRef(0);

  const load = useCallback(
    async (fromCursor: string, append: boolean) => {
      const my = ++gen.current;
      setLoading(true);
      setError(null);
      try {
        // Keep scanning until we have a screenful or the scan is complete:
        // SCAN with MATCH can return few or zero keys per call.
        let cur = fromCursor;
        let found: RedisKey[] = [];
        let rounds = 0;
        do {
          const r = await call<{ cursor: string; keys: RedisKey[] }>('redis.scan', {
            sessionId: ws.sessionId,
            cursor: cur,
            pattern: applied,
            count: 1000,
            kind: kind || null,
          });
          if (my !== gen.current) return;
          found = found.concat(r.keys);
          cur = r.cursor;
          rounds++;
        } while (cur !== '0' && found.length < 200 && rounds < 50);
        setKeys((prev) => {
          const merged = append ? prev.concat(found) : found;
          const seen = new Set<string>();
          return merged
            .filter((k) => {
              const id = k.b64 ?? k.text;
              if (seen.has(id)) return false; // SCAN may return a key twice
              seen.add(id);
              return true;
            })
            .sort((a, b) => a.text.localeCompare(b.text));
        });
        setCursor(cur);
      } catch (e) {
        if (my === gen.current) setError(errorMessage(e));
      } finally {
        if (my === gen.current) setLoading(false);
      }
    },
    [ws.sessionId, applied, kind],
  );

  useEffect(() => {
    void load('0', false);
    call<{ keyspace: { db: number; keys: number }[]; dbsize: number }>('redis.overview', { sessionId: ws.sessionId })
      .then(setOverview)
      .catch(() => {});
  }, [load, reloadKey]);

  const virtual = useVirtualizer({
    count: keys.length,
    getScrollElement: () => scroller.current,
    estimateSize: () => 30,
    overscan: 12,
  });
  const dbKeys = useMemo(() => new Map(overview?.keyspace.map((k) => [String(k.db), k.keys]) ?? []), [overview]);

  return (
    <aside className="sidebar redis-sidebar">
      <div className="sidebar-selectors">
        {ws.connection.options?.redisMode === 'cluster' ? (
          <span className="selector" title="Redis Cluster has a single database (0)">
            <span className="mono dim">cluster</span>
            <span className="mono">{overview ? `${compactCount(overview.dbsize)} keys` : 'db 0'}</span>
          </span>
        ) : (
          <label className="selector" title="Database">
            <span className="mono dim">db</span>
            <select value={ws.info.database} onChange={(e) => switchDatabase(ws.sessionId, e.target.value)}>
              {(ws.databases.length ? ws.databases : [ws.info.database]).map((d) => (
                <option key={d} value={d}>
                  {d}
                  {dbKeys.get(d) ? ` · ${compactCount(dbKeys.get(d)!)} keys` : ''}
                </option>
              ))}
            </select>
          </label>
        )}
      </div>
      <form
        className="redis-search"
        onSubmit={(e) => {
          e.preventDefault();
          setApplied(pattern.trim() || '*');
        }}
      >
        <div className="tree-search">
          <Search size={13} />
          <input
            className="tree-search-input mono"
            value={pattern}
            onChange={(e) => setPattern(e.target.value)}
            placeholder="Pattern, e.g. user:*"
            spellCheck={false}
          />
        </div>
        <select className="select select-sm" value={kind} onChange={(e) => setKind(e.target.value)} title="Type">
          <option value="">All types</option>
          {TYPES.map((t) => (
            <option key={t} value={t}>
              {t}
            </option>
          ))}
        </select>
      </form>
      <div className="redis-listbar">
        <span className="dim">
          {keys.length.toLocaleString()} {cursor !== '0' ? 'loaded' : keys.length === 1 ? 'key' : 'keys'}
          {overview ? ` of ${overview.dbsize.toLocaleString()}` : ''}
        </span>
        <div className="toolbar-spacer" />
        {onNewKey && (
          <IconButton label="New key" onClick={onNewKey}>
            <Plus size={14} />
          </IconButton>
        )}
        <IconButton label="Rescan" onClick={() => void load('0', false)}>
          <RefreshCw size={13} className={clsx(loading && 'spin')} />
        </IconButton>
      </div>
      {error && <div className="tree-error">{error}</div>}
      <div ref={scroller} className="redis-keys">
        <div style={{ height: virtual.getTotalSize(), position: 'relative' }}>
          {virtual.getVirtualItems().map((v) => {
            const k = keys[v.index];
            const active = selected && (selected.b64 ?? selected.text) === (k.b64 ?? k.text);
            const sep = k.text.lastIndexOf(':');
            return (
              <button
                key={v.key}
                className={clsx('redis-key', active && 'is-active')}
                style={{
                  position: 'absolute',
                  top: 0,
                  left: 0,
                  right: 0,
                  height: v.size,
                  transform: `translateY(${v.start}px)`,
                }}
                onClick={() => onSelect(k)}
                title={k.text}
              >
                <TypeBadge type={k.type} />
                <span className="redis-key-name mono">
                  {sep > 0 ? <span className="dim">{k.text.slice(0, sep + 1)}</span> : null}
                  {sep > 0 ? k.text.slice(sep + 1) : k.text}
                </span>
                {k.ttl >= 0 && <Clock size={11} className="dim" />}
              </button>
            );
          })}
        </div>
        {!loading && !keys.length && !error && <div className="tree-empty">No keys match {applied}.</div>}
        {cursor !== '0' && (
          <div className="redis-more">
            <Button size="sm" variant="ghost" onClick={() => void load(cursor, true)} disabled={loading}>
              {loading ? <Spinner size={11} /> : null} Load more
            </Button>
          </div>
        )}
      </div>
    </aside>
  );
}

// ---- key detail -----------------------------------------------------------------

function KeyView({ ws, k, onDeleted, onRenamed }: { ws: Workspace; k: RedisKey; onDeleted: () => void; onRenamed: (name: string) => void }) {
  const { toast, confirm } = useStore.getState();
  const [d, setD] = useState<KeyDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [ttlEdit, setTtlEdit] = useState<string | null>(null);
  const readOnly = ws.info.readOnly;
  const keyRef = { key: k.text, keyB64: k.b64 ?? null };

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setD(
        await call<KeyDetail>('redis.get', {
          sessionId: ws.sessionId,
          key: k.text,
          keyB64: k.b64 ?? null,
        }),
      );
    } catch (e) {
      setD(null);
      setError(errorMessage(e));
    } finally {
      setLoading(false);
    }
  }, [ws.sessionId, k.text, k.b64]);

  useEffect(() => {
    void load();
  }, [load]);

  const op = async (body: Record<string, unknown>, success?: string) => {
    try {
      await call('redis.op', { sessionId: ws.sessionId, ...keyRef, ...body });
      if (success) toast('success', success);
      await load();
      return true;
    } catch (e) {
      toast('error', 'Couldn’t save', errorMessage(e));
      return false;
    }
  };

  const del = async () => {
    const ok = await confirm({
      title: `Delete ${k.text}?`,
      message: 'The key and its value are removed from Redis. This can’t be undone.',
      confirmLabel: 'Delete',
      danger: true,
    });
    if (!ok) return;
    try {
      await call('redis.op', {
        sessionId: ws.sessionId,
        op: 'delete',
        keys: [k.text],
      });
      toast('success', 'Key deleted', k.text);
      onDeleted();
    } catch (e) {
      toast('error', 'Couldn’t delete', errorMessage(e));
    }
  };

  return (
    <div className="rk-view">
      <div className="rk-head">
        {d && <TypeBadge type={d.type} />}
        {renaming !== null ? (
          <form
            className="rk-rename"
            onSubmit={async (e) => {
              e.preventDefault();
              const name = renaming.trim();
              if (!name || name === k.text) return setRenaming(null);
              if (await op({ op: 'rename', newKey: name }, 'Key renamed')) {
                setRenaming(null);
                onRenamed(name);
              }
            }}
          >
            <input className="input input-sm mono" value={renaming} onChange={(e) => setRenaming(e.target.value)} autoFocus />
            <IconButton label="Rename" type="submit">
              <Check size={14} />
            </IconButton>
            <IconButton label="Cancel" onClick={() => setRenaming(null)}>
              <X size={14} />
            </IconButton>
          </form>
        ) : (
          <span className="rk-name mono" title={k.text}>
            {k.text}
          </span>
        )}
        <div className="toolbar-spacer" />
        {d && (
          <span className="rk-meta dim">
            {d.length !== undefined ? `${d.length.toLocaleString()} ${d.type === 'string' ? 'bytes' : 'items'} · ` : ''}
          </span>
        )}
        {d &&
          (ttlEdit !== null ? (
            <form
              className="rk-ttl-edit"
              onSubmit={async (e) => {
                e.preventDefault();
                const n = ttlEdit.trim() === '' ? null : Number(ttlEdit);
                if (n !== null && (!Number.isFinite(n) || n <= 0)) return toast('error', 'Enter seconds, or leave empty for no expiry');
                if (await op({ op: 'expire', ttl: n }, n ? `Expires in ${ttlLabel(n * 1000)}` : 'Expiry removed')) setTtlEdit(null);
              }}
            >
              <input
                className="input input-sm mono"
                value={ttlEdit}
                onChange={(e) => setTtlEdit(e.target.value.replace(/\D/g, ''))}
                placeholder="seconds"
                autoFocus
              />
              <IconButton label="Save TTL" type="submit">
                <Check size={14} />
              </IconButton>
              <IconButton label="Cancel" onClick={() => setTtlEdit(null)}>
                <X size={14} />
              </IconButton>
            </form>
          ) : (
            <button
              className="rk-ttl"
              disabled={readOnly}
              title={readOnly ? undefined : 'Set or remove the expiry'}
              onClick={() => setTtlEdit(d.ttl > 0 ? String(Math.round(d.ttl / 1000)) : '')}
            >
              <Clock size={12} /> {ttlLabel(d.ttl)}
            </button>
          ))}
        <IconButton label="Copy key name" onClick={() => copyText(k.text).then(() => toast('success', 'Copied'))}>
          <Copy size={13} />
        </IconButton>
        {!readOnly && (
          <IconButton label="Rename" onClick={() => setRenaming(k.text)}>
            <Pencil size={13} />
          </IconButton>
        )}
        <IconButton label="Reload" onClick={() => void load()}>
          <RefreshCw size={13} className={clsx(loading && 'spin')} />
        </IconButton>
        {!readOnly && (
          <IconButton label="Delete key" onClick={del} className="danger-hover">
            <Trash2 size={13} />
          </IconButton>
        )}
      </div>
      {error && (
        <div className="rk-body">
          <EmptyState title="Couldn’t read this key">{error}</EmptyState>
        </div>
      )}
      {!d && loading && (
        <div className="rk-body rk-center">
          <Spinner />
        </div>
      )}
      {d && (
        <div className="rk-body">
          {d.truncated && (
            <div className="callout">
              Showing the first {d.type === 'string' ? '1 MB' : `${(d.items?.length ?? 0).toLocaleString()} items`} of {d.length?.toLocaleString()}.
              Use the console for the rest.
            </div>
          )}
          {d.unsupported && <div className="callout">{d.unsupported}</div>}
          {(d.type === 'string' || d.type === 'ReJSON-RL') && d.value && (
            <StringEditor
              value={d.value}
              readOnly={readOnly || d.type !== 'string' || !!d.truncated || !!d.value.b64}
              onSave={(value) => op({ op: 'setString', value }, 'Saved')}
            />
          )}
          {d.type === 'hash' && (
            <PairTable
              head={['Field', 'Value']}
              rows={(d.items ?? []).map((i) => [(i.field as Bin).text, (i.value as Bin).text])}
              readOnly={readOnly}
              onSave={(row, [field, value]) =>
                op({
                  op: 'hashSet',
                  field,
                  value,
                  oldField: row === null ? null : (d.items![row].field as Bin).text,
                })
              }
              onDelete={(row) =>
                op({
                  op: 'hashDel',
                  fields: [(d.items![row].field as Bin).text],
                })
              }
              addLabel="Add field"
            />
          )}
          {d.type === 'list' && (
            <PairTable
              head={['#', 'Value']}
              rows={(d.items ?? []).map((i) => [String(i.index), (i.value as Bin).text])}
              readOnly={readOnly}
              keyFixed
              onSave={(row, [, value]) =>
                row === null
                  ? op({ op: 'listPush', value, head: false })
                  : op({
                      op: 'listSet',
                      index: row,
                      value,
                      expected: (d.items![row].value as Bin).text,
                    })
              }
              onDelete={(row) =>
                op({
                  op: 'listRemove',
                  index: row,
                  expected: (d.items![row].value as Bin).text,
                })
              }
              addLabel="Append"
            />
          )}
          {d.type === 'set' && (
            <PairTable
              head={['Member']}
              rows={(d.items ?? []).map((i) => [(i.member as Bin).text])}
              readOnly={readOnly}
              onSave={async (row, [member]) => {
                if (row !== null) {
                  const old = (d.items![row].member as Bin).text;
                  if (old === member) return true;
                  if (!(await op({ op: 'setAdd', members: [member] }))) return false;
                  return op({ op: 'setRemove', members: [old] });
                }
                return op({ op: 'setAdd', members: [member] });
              }}
              onDelete={(row) =>
                op({
                  op: 'setRemove',
                  members: [(d.items![row].member as Bin).text],
                })
              }
              addLabel="Add member"
            />
          )}
          {d.type === 'zset' && (
            <PairTable
              head={['Member', 'Score']}
              rows={(d.items ?? []).map((i) => [(i.member as Bin).text, String(i.score)])}
              readOnly={readOnly}
              numericSecond
              onSave={async (row, [member, score]) => {
                const n = Number(score);
                if (!Number.isFinite(n)) {
                  toast('error', 'Score must be a number');
                  return false;
                }
                if (row !== null) {
                  const old = (d.items![row].member as Bin).text;
                  if (old !== member && !(await op({ op: 'zsetRemove', members: [old] }))) return false;
                }
                return op({ op: 'zsetAdd', member, score: n });
              }}
              onDelete={(row) =>
                op({
                  op: 'zsetRemove',
                  members: [(d.items![row].member as Bin).text],
                })
              }
              addLabel="Add member"
            />
          )}
          {d.type === 'stream' && (
            <table className="rk-table">
              <thead>
                <tr>
                  <th>ID</th>
                  <th>Fields</th>
                </tr>
              </thead>
              <tbody>
                {(d.items ?? []).map((e) => (
                  <tr key={String(e.id)}>
                    <td className="mono dim">{String(e.id)}</td>
                    <td className="mono">
                      {(e.fields as [Bin, Bin][]).map(([f, v], i) => (
                        <span key={i} className="rk-field">
                          <span className="dim">{f.text}</span>={v.text}
                        </span>
                      ))}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      )}
    </div>
  );
}

function StringEditor({ value, readOnly, onSave }: { value: Bin; readOnly: boolean; onSave: (v: string) => Promise<boolean> }) {
  const [text, setText] = useState(value.text);
  const [saving, setSaving] = useState(false);
  useEffect(() => setText(value.text), [value.text]);
  const pretty = tryPrettyJson(text);
  const dirty = text !== value.text;
  return (
    <div className="rk-string">
      {value.b64 && <div className="callout">This value is binary. It’s shown as text with unreadable bytes replaced, and can’t be edited here.</div>}
      <textarea
        className="input mono rk-textarea"
        value={text}
        readOnly={readOnly}
        spellCheck={false}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === 's' && dirty && !readOnly) {
            e.preventDefault();
            void onSave(text);
          }
        }}
      />
      <div className="rk-actions">
        {pretty && pretty !== text && !readOnly && (
          <Button size="sm" variant="ghost" onClick={() => setText(pretty)}>
            Format JSON
          </Button>
        )}
        <span className="dim">{new TextEncoder().encode(text).length.toLocaleString()} bytes</span>
        <div className="toolbar-spacer" />
        {!readOnly && (
          <>
            <Button size="sm" variant="ghost" disabled={!dirty} onClick={() => setText(value.text)}>
              Revert
            </Button>
            <Button
              size="sm"
              variant="primary"
              disabled={!dirty || saving}
              onClick={async () => {
                setSaving(true);
                await onSave(text);
                setSaving(false);
              }}
            >
              Save
            </Button>
          </>
        )}
      </div>
    </div>
  );
}

/** Editable table of one or two columns (hash fields, list items, set members, zset scores). */
function PairTable({
  head,
  rows,
  readOnly,
  onSave,
  onDelete,
  addLabel,
  keyFixed,
  numericSecond,
}: {
  head: string[];
  rows: string[][];
  readOnly: boolean;
  onSave: (row: number | null, values: string[]) => Promise<boolean>;
  onDelete: (row: number) => Promise<boolean>;
  addLabel: string;
  keyFixed?: boolean;
  numericSecond?: boolean;
}) {
  const [edit, setEdit] = useState<{
    row: number | null;
    values: string[];
  } | null>(null);
  const [filter, setFilter] = useState('');
  const cols = head.length;
  const shown = rows.map((r, i) => ({ r, i })).filter(({ r }) => !filter || r.some((c) => c.toLowerCase().includes(filter.toLowerCase())));

  const save = async () => {
    if (!edit) return;
    if (await onSave(edit.row, edit.values)) setEdit(null);
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      void save();
    }
    if (e.key === 'Escape') {
      e.stopPropagation();
      setEdit(null);
    }
  };
  const editor = (values: string[]) => (
    <>
      {values.map((v, c) =>
        keyFixed && c === 0 ? (
          <td key={c} className="mono dim">
            {edit?.row === null ? 'new' : v}
          </td>
        ) : (
          <td key={c}>
            <textarea
              className={clsx('input input-sm mono rk-cell-input', numericSecond && c === 1 && 'is-num')}
              rows={Math.min(6, Math.max(1, v.split('\n').length))}
              value={v}
              autoFocus={c === (keyFixed ? 1 : 0)}
              onKeyDown={onKey}
              onChange={(e) =>
                setEdit((x) =>
                  x
                    ? {
                        ...x,
                        values: x.values.map((y, j) => (j === c ? e.target.value : y)),
                      }
                    : x,
                )
              }
            />
          </td>
        ),
      )}
      <td className="rk-row-actions">
        <IconButton label="Save" onClick={() => void save()}>
          <Check size={13} />
        </IconButton>
        <IconButton label="Cancel" onClick={() => setEdit(null)}>
          <X size={13} />
        </IconButton>
      </td>
    </>
  );

  return (
    <div className="rk-pairs">
      <div className="rk-pairs-bar">
        <div className="tree-search">
          <Search size={12} />
          <input className="tree-search-input" value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Filter" />
        </div>
        <div className="toolbar-spacer" />
        {!readOnly && (
          <Button
            size="sm"
            onClick={() =>
              setEdit({
                row: null,
                values: head.map((h) => (h === '#' ? '' : '')),
              })
            }
          >
            <Plus size={12} /> {addLabel}
          </Button>
        )}
      </div>
      <table className="rk-table">
        <thead>
          <tr>
            {head.map((h) => (
              <th key={h} className={h === '#' ? 'rk-idx' : undefined}>
                {h}
              </th>
            ))}
            <th className="rk-row-actions" />
          </tr>
        </thead>
        <tbody>
          {edit?.row === null && <tr className="is-editing">{editor(edit.values)}</tr>}
          {shown.map(({ r, i }) =>
            edit?.row === i ? (
              <tr key={i} className="is-editing">
                {editor(edit.values)}
              </tr>
            ) : (
              <tr key={i} onDoubleClick={() => !readOnly && setEdit({ row: i, values: [...r] })}>
                {r.slice(0, cols).map((c, j) => (
                  <td key={j} className={clsx('mono', (keyFixed && j === 0) || (numericSecond && j === 1) ? 'rk-num' : undefined)}>
                    {c}
                  </td>
                ))}
                <td className="rk-row-actions">
                  {!readOnly && (
                    <>
                      <IconButton label="Edit" onClick={() => setEdit({ row: i, values: [...r] })}>
                        <Pencil size={12} />
                      </IconButton>
                      <IconButton label="Remove" onClick={() => void onDelete(i)} className="danger-hover">
                        <Trash2 size={12} />
                      </IconButton>
                    </>
                  )}
                </td>
              </tr>
            ),
          )}
        </tbody>
      </table>
      {!rows.length && <div className="tree-empty">Empty</div>}
    </div>
  );
}

// ---- console --------------------------------------------------------------------

interface ConsoleEntry {
  id: number;
  line: string;
  out?: string;
  error?: boolean;
  ms?: number;
}

function RedisConsole({ ws, active, onWrite }: { ws: Workspace; active: boolean; onWrite: () => void }) {
  const { confirm } = useStore.getState();
  const [entries, setEntries] = useState<ConsoleEntry[]>([]);
  const [line, setLine] = useState('');
  const [busy, setBusy] = useState(false);
  const history = useRef<string[]>([]);
  const hpos = useRef(-1);
  const input = useRef<HTMLInputElement>(null);
  const log = useRef<HTMLDivElement>(null);
  const seq = useRef(0);

  useEffect(() => {
    if (active) input.current?.focus();
  }, [active]);
  useEffect(() => {
    log.current?.scrollTo({ top: log.current.scrollHeight });
  }, [entries]);

  const run = async (text: string, confirmed = false) => {
    const id = ++seq.current;
    if (!confirmed) setEntries((e) => [...e, { id, line: text }]);
    setBusy(true);
    try {
      const r = await call<{
        reply: Reply;
        durationMs: number;
        write?: boolean;
      }>('redis.command', { sessionId: ws.sessionId, line: text, confirmed });
      setEntries((e) =>
        e.map((x) =>
          x.id === id || (confirmed && x.line === text && x.out === undefined)
            ? {
                ...x,
                out: renderReply(r.reply),
                error: r.reply.t === 'error',
                ms: r.durationMs,
              }
            : x,
        ),
      );
      if (r.write) onWrite();
    } catch (err) {
      if (err instanceof RpcError && err.code === 'confirm_required' && !confirmed) {
        const ok = await confirm({
          title: 'Run this command?',
          message: err.message,
          code: text,
          confirmLabel: 'Run',
          danger: true,
        });
        if (ok) return run(text, true);
        setEntries((e) => e.map((x) => (x.id === id ? { ...x, out: '(cancelled)', error: true } : x)));
      } else {
        setEntries((e) =>
          e.map((x) => (x.id === id || (confirmed && x.line === text && x.out === undefined) ? { ...x, out: errorMessage(err), error: true } : x)),
        );
      }
    } finally {
      setBusy(false);
      input.current?.focus();
    }
  };

  return (
    <div className="redis-console" onClick={() => window.getSelection()?.isCollapsed && input.current?.focus()}>
      <div ref={log} className="redis-log mono">
        {!entries.length && (
          <div className="dim redis-hint">
            Type a Redis command and press Enter, e.g. <b>HGETALL user:1</b> or <b>INFO memory</b>. Quote values with spaces: SET greeting "hello
            world". ↑/↓ for history.
          </div>
        )}
        {entries.map((e) => (
          <div key={e.id} className="redis-entry">
            <div className="redis-cmd">
              <span className="redis-prompt">{ws.info.database}&gt;</span> {e.line}
              {e.ms !== undefined && <span className="dim redis-ms">{e.ms < 1 ? '<1' : Math.round(e.ms)} ms</span>}
            </div>
            {e.out === undefined ? <Spinner size={11} /> : <pre className={clsx('redis-out', e.error && 'is-error')}>{e.out}</pre>}
          </div>
        ))}
      </div>
      <form
        className="redis-input"
        onSubmit={(e) => {
          e.preventDefault();
          const text = line.trim();
          if (!text || busy) return;
          if (text.toLowerCase() === 'clear') {
            setEntries([]);
            setLine('');
            return;
          }
          history.current = [text, ...history.current.filter((h) => h !== text)].slice(0, 200);
          hpos.current = -1;
          setLine('');
          void run(text);
        }}
      >
        <span className="redis-prompt mono">{ws.info.database}&gt;</span>
        <input
          ref={input}
          className="mono"
          value={line}
          spellCheck={false}
          autoComplete="off"
          placeholder={ws.info.readOnly ? 'Read-only: only commands that read are allowed' : 'Command'}
          onChange={(e) => setLine(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'ArrowUp' && history.current.length) {
              e.preventDefault();
              hpos.current = Math.min(history.current.length - 1, hpos.current + 1);
              setLine(history.current[hpos.current]);
            } else if (e.key === 'ArrowDown') {
              e.preventDefault();
              hpos.current = Math.max(-1, hpos.current - 1);
              setLine(hpos.current < 0 ? '' : history.current[hpos.current]);
            }
          }}
        />
        {busy && <Spinner size={12} />}
      </form>
    </div>
  );
}

// ---- server ---------------------------------------------------------------------

interface ClusterNodeInfo {
  address: string;
  role: 'primary' | 'replica' | 'failed';
  slots: string;
  keys: number | null;
  memory: string | null;
}

interface Overview {
  info: Record<string, string>;
  keyspace: { db: number; keys: number }[];
  dbsize: number;
  cluster?: {
    state: string | null;
    size: string | null;
    knownNodes: string | null;
    slotsAssigned: string | null;
    nodes: ClusterNodeInfo[];
  };
}

function ServerInfo({ ws }: { ws: Workspace }) {
  const [o, setO] = useState<Overview | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    call<Overview>('redis.overview', { sessionId: ws.sessionId })
      .then(setO)
      .catch((e) => setError(errorMessage(e)));
  }, [ws.sessionId]);
  if (error) return <EmptyState title="Couldn’t read server info">{error}</EmptyState>;
  if (!o) return <Spinner />;
  if (o.cluster) {
    const c = o.cluster;
    const crows: [string, ReactNode][] = [
      ['Server', `${ws.info.flavor} ${ws.info.version}`],
      ['Mode', 'cluster'],
      ['State', c.state === 'ok' ? 'ok' : <span className="text-danger">{c.state ?? 'unknown'}</span>],
      ['Primaries', c.size ?? '—'],
      ['Nodes', c.knownNodes ?? '—'],
      ['Slots assigned', c.slotsAssigned ? `${c.slotsAssigned} of 16384` : '—'],
      ['Keys', o.dbsize.toLocaleString()],
      ['OS', o.info.os ?? '—'],
    ];
    return (
      <div className="redis-server">
        <dl className="redis-dl">
          {crows.map(([k, v]) => (
            <div key={k}>
              <dt>{k}</dt>
              <dd>{v}</dd>
            </div>
          ))}
        </dl>
        <h3>Nodes</h3>
        <table className="rk-table">
          <thead>
            <tr>
              <th>Address</th>
              <th>Role</th>
              <th>Slots</th>
              <th>Keys</th>
              <th>Memory</th>
            </tr>
          </thead>
          <tbody>
            {c.nodes.map((n) => (
              <tr key={n.address}>
                <td className="mono">{n.address}</td>
                <td className={n.role === 'failed' ? 'text-danger' : n.role === 'replica' ? 'dim' : undefined}>{n.role}</td>
                <td className="mono dim">{n.slots || '—'}</td>
                <td className="mono rk-num">{n.keys == null ? '—' : n.keys.toLocaleString()}</td>
                <td className="mono rk-num">{n.memory ?? '—'}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    );
  }
  const rows: [string, ReactNode][] = [
    ['Server', `${ws.info.flavor} ${ws.info.version}`],
    ['Mode', o.info.redis_mode ?? '—'],
    ['Role', o.info.role ?? '—'],
    ['Memory used', o.info.used_memory_human ?? '—'],
    ['Max memory', o.info.maxmemory_human && o.info.maxmemory_human !== '0B' ? `${o.info.maxmemory_human} (${o.info.maxmemory_policy})` : 'No limit'],
    ['Clients', o.info.connected_clients ?? '—'],
    ['Uptime', o.info.uptime_in_days ? `${o.info.uptime_in_days} days` : '—'],
    ['Commands run', Number(o.info.total_commands_processed ?? 0).toLocaleString()],
    ['OS', o.info.os ?? '—'],
  ];
  return (
    <div className="redis-server">
      <dl className="redis-dl">
        {rows.map(([k, v]) => (
          <div key={k}>
            <dt>{k}</dt>
            <dd>{v}</dd>
          </div>
        ))}
      </dl>
      <h3>Databases</h3>
      <table className="rk-table">
        <thead>
          <tr>
            <th>DB</th>
            <th>Keys</th>
          </tr>
        </thead>
        <tbody>
          {o.keyspace.map((k) => (
            <tr key={k.db}>
              <td className="mono">{k.db}</td>
              <td className="mono rk-num">{k.keys.toLocaleString()}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

// ---- new key --------------------------------------------------------------------

function NewKeyModal({ ws, onClose, onCreated }: { ws: Workspace; onClose: () => void; onCreated: (key: string, type: string) => void }) {
  const { toast } = useStore.getState();
  const [key, setKey] = useState('');
  const [kind, setKind] = useState('string');
  const [field, setField] = useState('');
  const [value, setValue] = useState('');
  const [score, setScore] = useState('0');
  const [ttl, setTtl] = useState('');
  const [busy, setBusy] = useState(false);
  const create = async () => {
    setBusy(true);
    try {
      await call('redis.op', {
        sessionId: ws.sessionId,
        op: 'create',
        key,
        kind,
        value,
        field: kind === 'hash' ? field || 'field' : null,
        score: kind === 'zset' ? Number(score) || 0 : null,
        ttl: ttl ? Number(ttl) : null,
      });
      toast('success', 'Key created', key);
      onCreated(key, kind);
    } catch (e) {
      toast('error', 'Couldn’t create the key', errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title="New key"
      onClose={onClose}
      width={480}
      footer={
        <>
          <div className="toolbar-spacer" />
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" disabled={!key || busy} onClick={create}>
            Create
          </Button>
        </>
      }
    >
      <form
        className="conn-form"
        onSubmit={(e) => {
          e.preventDefault();
          if (key) void create();
        }}
      >
        <Field label="Key">
          <input className="input mono" value={key} onChange={(e) => setKey(e.target.value)} placeholder="user:42" autoFocus spellCheck={false} />
        </Field>
        <Field label="Type">
          <Segmented
            value={kind}
            onChange={setKind}
            options={['string', 'hash', 'list', 'set', 'zset'].map((t) => ({
              value: t,
              label: t,
            }))}
          />
        </Field>
        {kind === 'hash' && (
          <Field label="First field">
            <input className="input mono" value={field} onChange={(e) => setField(e.target.value)} placeholder="name" spellCheck={false} />
          </Field>
        )}
        <Field label={kind === 'string' ? 'Value' : kind === 'hash' ? 'Its value' : kind === 'list' ? 'First item' : 'First member'}>
          <textarea className="input mono" rows={3} value={value} onChange={(e) => setValue(e.target.value)} spellCheck={false} />
        </Field>
        <div className="form-grid">
          {kind === 'zset' && (
            <Field label="Score">
              <input className="input mono" value={score} onChange={(e) => setScore(e.target.value)} />
            </Field>
          )}
          <Field label="Expires after (seconds)" className={kind === 'zset' ? 'span-2' : 'span-3'}>
            <input className="input mono" value={ttl} onChange={(e) => setTtl(e.target.value.replace(/\D/g, ''))} placeholder="Never" />
          </Field>
        </div>
        <button type="submit" hidden />
      </form>
    </Modal>
  );
}
