import clsx from 'clsx';
import { Container, Copy, FolderOpen, MoreHorizontal, Pencil, Plus, Radar, RefreshCw, Search, Trash2, Zap, Lock, CheckCircle2 } from 'lucide-react';
import { useMemo, useState } from 'react';
import { relativeTime } from '../lib/format';
import { defaultPort } from '../lib/dialect';
import { pickOpenFile, mod } from '../lib/platform';
import { call, errorMessage } from '../lib/rpc';
import { isFileEngine, type ConnectionConfig, type Discovered } from '../lib/types';
import { useStore } from '../state/store';
import { blankConnection, ENGINE_COLOR } from './ConnectionForm';
import { Button, EngineIcon, EnvBadge, IconButton, Kbd, LogoMark, Spinner, useMenu } from './ui';

function subtitle(c: ConnectionConfig): string {
  if (isFileEngine(c.engine)) return c.filePath.replace(/^\/Users\/[^/]+/, '~');
  const host = c.host || 'localhost';
  const port = c.port ? `:${c.port}` : '';
  const via = c.sshEnabled && c.sshHost ? ` · via ${c.sshHost}` : '';
  return `${c.username ? c.username + '@' : ''}${host}${port}${c.database ? ' / ' + c.database : ''}${via}`;
}

function radarName(d: Discovered): string {
  if (d.container) return d.container;
  const base = d.flavor === 'PostgreSQL' ? 'Postgres' : d.flavor;
  return d.port === defaultPort(d.engine) ? `Local ${base}` : `Local ${base} :${d.port}`;
}

export function Home() {
  const connections = useStore((s) => s.connections);
  const radar = useStore((s) => s.radar);
  const connecting = useStore((s) => s.connecting);
  const license = useStore((s) => s.license);
  const workspaces = useStore((s) => s.workspaces);
  const { connect, openModal, deleteConnection, scanRadar, saveConnection, toast, handleLimitError } = useStore.getState();
  const [q, setQ] = useState('');
  const [menu, openMenu] = useMenu();
  const [busyRadar, setBusyRadar] = useState<string | null>(null);

  const filtered = useMemo(() => {
    const f = q.trim().toLowerCase();
    return connections.filter((c) => !f || `${c.name} ${subtitle(c)} ${c.groupName} ${c.env}`.toLowerCase().includes(f));
  }, [connections, q]);

  const groups = useMemo(() => {
    const m = new Map<string, ConnectionConfig[]>();
    for (const c of filtered) {
      const g = c.groupName || '';
      m.set(g, [...(m.get(g) ?? []), c]);
    }
    return [...m.entries()].sort((a, b) => (a[0] === '' ? -1 : b[0] === '' ? 1 : a[0].localeCompare(b[0])));
  }, [filtered]);

  const unsaved = radar.results.filter((d) => !d.alreadySaved);

  const connectDiscovered = async (d: Discovered) => {
    const cfg: ConnectionConfig = {
      ...blankConnection(d.engine),
      name: radarName(d),
      host: d.host,
      port: d.port,
      username: d.username ?? '',
      database: d.database ?? '',
      sslMode: 'prefer',
      env: 'local',
      color: d.flavor === 'MariaDB' ? '#A371F7' : ENGINE_COLOR[d.engine],
    };
    // Logging in happens only now, because the user clicked Connect.
    setBusyRadar(d.id);
    try {
      await call('connections.test', {
        connection: { ...cfg, name: cfg.name || 'test' },
        password: d.password ?? '',
      });
    } catch (e) {
      setBusyRadar(null);
      openModal({
        kind: 'connection',
        preset: cfg,
        presetPassword: d.password ?? '',
      });
      toast('info', 'Almost there', `${errorMessage(e)} — check the details and connect.`);
      return;
    }
    try {
      const saved = await saveConnection(cfg, d.password ?? undefined);
      await connect(saved.id);
      void scanRadar();
    } catch (e) {
      if (!handleLimitError(e)) toast('error', 'Couldn’t save connection', errorMessage(e));
    } finally {
      setBusyRadar(null);
    }
  };

  const openSqliteFile = async () => {
    const p = await pickOpenFile('Open database file', [
      {
        name: 'SQLite or DuckDB',
        extensions: ['sqlite', 'sqlite3', 'db', 'db3', 's3db', 'duckdb', 'ddb'],
      },
      { name: 'All files', extensions: ['*'] },
    ]);
    if (!p) return;
    const engine = /\.(duckdb|ddb)$/i.test(p) ? 'duckdb' : 'sqlite';
    const existing = connections.find((c) => isFileEngine(c.engine) && c.filePath === p);
    if (existing) return void connect(existing.id);
    try {
      const saved = await saveConnection({
        ...blankConnection(engine),
        filePath: p,
        name: p.split('/').pop() ?? 'Database',
      });
      await connect(saved.id);
    } catch (e) {
      if (!handleLimitError(e)) toast('error', 'Couldn’t open file', errorMessage(e));
    }
  };

  const duplicate = async (c: ConnectionConfig) => {
    try {
      await saveConnection({
        ...c,
        id: '',
        name: `${c.name} copy`,
        createdAt: 0,
      });
    } catch (e) {
      if (!handleLimitError(e)) toast('error', 'Couldn’t duplicate', errorMessage(e));
    }
  };

  const editConnection = async (c: ConnectionConfig) => {
    const full = await call<ConnectionConfig>('connections.get', { id: c.id });
    openModal({ kind: 'connection', connection: full });
  };

  const limit = license?.limits.connections;
  const isEmpty = connections.length === 0;

  return (
    <div className="home">
      <div className="home-inner">
        <header className="home-header">
          <div className="home-brand">
            <LogoMark size={30} />
            <div>
              <h1>{isEmpty ? 'Welcome to Rowglow' : 'Connections'}</h1>
              <p className="dim">
                {isEmpty
                  ? 'Ask your data anything. Start with a database Rowglow found on this computer, or add your own.'
                  : `${connections.length} saved · ${workspaces.length} open`}
              </p>
            </div>
          </div>
          <div className="home-actions">
            {!isEmpty && (
              <div className="home-search">
                <Search size={13} />
                <input placeholder="Filter connections" value={q} onChange={(e) => setQ(e.target.value)} />
              </div>
            )}
            <Button onClick={openSqliteFile}>
              <FolderOpen size={14} /> Open file…
            </Button>
            <Button variant="primary" onClick={() => openModal({ kind: 'connection' })}>
              <Plus size={14} /> New connection
            </Button>
          </div>
        </header>

        <section className="radar">
          <div className="section-head">
            <div className="section-title">
              <Radar size={15} className={clsx(radar.scanning && 'pulse')} />
              Local Radar
              <span className="section-sub">
                {radar.scanning
                  ? 'Scanning ports and Docker…'
                  : unsaved.length
                    ? `Found ${unsaved.length} on this computer`
                    : 'Databases running on this computer show up here'}
              </span>
            </div>
            <IconButton label="Scan again" onClick={() => scanRadar()} disabled={radar.scanning}>
              <RefreshCw size={13} className={clsx(radar.scanning && 'spin')} />
            </IconButton>
          </div>
          {radar.scanning && !radar.results.length ? (
            <div className="radar-grid">
              {[0, 1].map((i) => (
                <div key={i} className="radar-card is-skeleton" />
              ))}
            </div>
          ) : unsaved.length ? (
            <div className="radar-grid">
              {unsaved.map((d) => (
                <div key={d.id} className="radar-card">
                  <EngineIcon engine={d.engine} flavor={d.flavor} size={34} />
                  <div className="radar-info">
                    <div className="radar-name">{radarName(d)}</div>
                    <div className="radar-meta">
                      {d.flavor}
                      {d.version ? ` ${d.version}` : ''} · :{d.port}
                    </div>
                    <div className="radar-tags">
                      <span className="tag">
                        {d.source === 'Docker' ? <Container size={10} /> : <Zap size={10} />} {d.source}
                      </span>
                      {d.password ? (
                        <span className="tag tag-ok" title="Prefilled from the container's settings">
                          <CheckCircle2 size={10} /> Credentials from Docker
                        </span>
                      ) : null}
                      {d.note ? (
                        <span className={clsx('tag', d.note === 'Password required' && 'tag-warn')}>
                          {d.note === 'Password required' ? <Lock size={10} /> : null} {d.note}
                        </span>
                      ) : null}
                    </div>
                  </div>
                  <Button size="sm" variant="primary" onClick={() => connectDiscovered(d)} disabled={busyRadar === d.id}>
                    {busyRadar === d.id ? <Spinner size={12} /> : null}
                    Connect
                  </Button>
                </div>
              ))}
            </div>
          ) : (
            !radar.scanning && (
              <div className="radar-empty dim">
                {radar.results.length
                  ? 'Every database we found is already saved below.'
                  : radar.scannedAt
                    ? 'Nothing found on common ports. Start a database (Postgres, MySQL, SQL Server, Redis, MongoDB) or a Docker container and scan again.'
                    : 'Click scan to look for databases running on this computer. Nothing leaves your machine.'}
              </div>
            )
          )}
        </section>

        {groups.map(([group, list]) => (
          <section key={group} className="conn-section">
            <div className="section-head">
              <div className="section-title">{group || (isEmpty ? '' : 'Saved')}</div>
              {!group && limit !== null && limit !== undefined && (
                <span className="section-sub">
                  {connections.length}/{limit} on the free plan
                </span>
              )}
            </div>
            <div className="conn-grid">
              {list.map((c) => {
                const open = workspaces.some((w) => w.connection.id === c.id);
                return (
                  <div
                    key={c.id}
                    className={clsx('conn-card', open && 'is-open')}
                    role="button"
                    tabIndex={0}
                    style={{
                      ['--conn-color' as string]: c.color || 'var(--accent)',
                    }}
                    onClick={() => connect(c.id)}
                    onKeyDown={(e) => e.key === 'Enter' && connect(c.id)}
                    onContextMenu={(e) =>
                      openMenu(e, [
                        { label: 'Connect', onSelect: () => connect(c.id) },
                        {
                          label: 'Edit…',
                          icon: <Pencil size={14} />,
                          onSelect: () => editConnection(c),
                        },
                        {
                          label: 'Duplicate',
                          icon: <Copy size={14} />,
                          onSelect: () => duplicate(c),
                        },
                        { separator: true },
                        {
                          label: 'Delete…',
                          icon: <Trash2 size={14} />,
                          danger: true,
                          onSelect: () => deleteConnection(c),
                        },
                      ])
                    }
                  >
                    <div className="conn-stripe" />
                    <EngineIcon engine={c.engine} size={32} />
                    <div className="conn-info">
                      <div className="conn-name">
                        {c.name}
                        {c.readOnly && <Lock size={11} className="dim" />}
                      </div>
                      <div className="conn-sub mono">{subtitle(c)}</div>
                      <div className="conn-foot">
                        <EnvBadge env={c.env} compact />
                        {open ? (
                          <span className="tag tag-ok">Open</span>
                        ) : c.lastUsedAt ? (
                          <span className="dim">{relativeTime(c.lastUsedAt)}</span>
                        ) : null}
                      </div>
                    </div>
                    {connecting === c.id ? (
                      <Spinner size={14} />
                    ) : (
                      <IconButton
                        label="More"
                        className="conn-more"
                        onClick={(e) => {
                          e.stopPropagation();
                          openMenu(e, [
                            {
                              label: 'Edit…',
                              icon: <Pencil size={14} />,
                              onSelect: () => editConnection(c),
                            },
                            {
                              label: 'Duplicate',
                              icon: <Copy size={14} />,
                              onSelect: () => duplicate(c),
                            },
                            { separator: true },
                            {
                              label: 'Delete…',
                              icon: <Trash2 size={14} />,
                              danger: true,
                              onSelect: () => deleteConnection(c),
                            },
                          ]);
                        }}
                      >
                        <MoreHorizontal size={15} />
                      </IconButton>
                    )}
                  </div>
                );
              })}
            </div>
          </section>
        ))}

        {isEmpty && !unsaved.length && !radar.scanning && (
          <div className="home-empty">
            <p>
              Add a connection with <Kbd>{mod}N</Kbd>, open a SQLite or DuckDB file, or paste a connection URL.
            </p>
          </div>
        )}
        <footer className="home-footer dim">
          <span>
            <Kbd>{mod}K</Kbd> open anything
          </span>
          <span>
            <Kbd>{mod}N</Kbd> new connection
          </span>
          <span>
            <Kbd>{mod},</Kbd> settings
          </span>
        </footer>
      </div>
      {menu}
    </div>
  );
}
