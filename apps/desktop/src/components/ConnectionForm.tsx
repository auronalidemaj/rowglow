import clsx from 'clsx';
import { CheckCircle2, ChevronDown, ChevronRight, FilePlus2, FolderOpen, KeyRound, Link2, XCircle } from 'lucide-react';
import { useState } from 'react';
import { parseConnectionUrl } from '../lib/connurl';
import { defaultPort, ENGINE_LABELS } from '../lib/dialect';
import { keychainName, pickOpenFile, pickSaveFile } from '../lib/platform';
import { call, errorMessage } from '../lib/rpc';
import { isFileEngine, type ConnectionConfig, type ConnOptions, type Engine, type MssqlAuth, type RedisMode, type ServerInfo } from '../lib/types';
import { useStore } from '../state/store';
import { Button, EngineIcon, Field, Modal, Segmented, Spinner, Toggle } from './ui';

export const COLORS = ['#F2A93B', '#E5534B', '#2BB3A3', '#4C8DF6', '#A371F7', '#E879F9', '#22C55E', '#94A3B8'];

export const ENGINE_COLOR: Record<Engine, string> = {
  postgres: '#4C8DF6',
  mysql: '#F2A93B',
  sqlite: '#2BB3A3',
  mssql: '#E5534B',
  duckdb: '#F2A93B',
  redis: '#E5534B',
  mongodb: '#22C55E',
};

export function blankConnection(engine: Engine = 'postgres'): ConnectionConfig {
  return {
    id: '',
    name: '',
    engine,
    host: isFileEngine(engine) ? '' : '127.0.0.1',
    port: defaultPort(engine),
    username: engine === 'mysql' ? 'root' : engine === 'mssql' ? 'sa' : '',
    database: engine === 'postgres' ? 'postgres' : '',
    sslMode: 'prefer',
    filePath: '',
    color: ENGINE_COLOR[engine],
    env: 'local',
    readOnly: false,
    groupName: '',
    createdAt: 0,
    updatedAt: 0,
    hasPassword: false,
    sshEnabled: false,
    sshHost: '',
    sshPort: 22,
    sshUser: '',
    sshAuth: 'key',
    sshKeyPath: '',
  };
}

type EngineChoice = Engine | 'mariadb';
const CHOICES: EngineChoice[] = ['postgres', 'mysql', 'mariadb', 'mssql', 'sqlite', 'duckdb', 'redis', 'mongodb'];

const choiceLabel = (f: EngineChoice) => (f === 'mariadb' ? 'MariaDB' : ENGINE_LABELS[f]);

const IS_WINDOWS = typeof navigator !== 'undefined' && /Win/.test(navigator.platform);

const MSSQL_AUTH: { value: MssqlAuth; label: string; windowsOnly?: boolean }[] = [
  { value: 'sql', label: 'SQL Server login' },
  { value: 'integrated', label: 'Current Windows user', windowsOnly: true },
  { value: 'windows', label: 'Windows account', windowsOnly: true },
  { value: 'entra-cli', label: 'Microsoft Entra ID — Azure CLI' },
  { value: 'entra-sp', label: 'Microsoft Entra ID — service principal' },
];

function userPlaceholder(engine: Engine): string {
  switch (engine) {
    case 'mysql':
      return 'root';
    case 'mssql':
      return 'sa';
    case 'redis':
      return 'default (optional)';
    case 'mongodb':
      return 'optional';
    default:
      return 'postgres';
  }
}

export function ConnectionForm({
  initial,
  preset,
  presetPassword,
}: {
  initial?: ConnectionConfig;
  preset?: Partial<ConnectionConfig>;
  presetPassword?: string;
}) {
  const { openModal, saveConnection, connect, toast, handleLimitError, trustSsh } = useStore.getState();
  const [c, setC] = useState<ConnectionConfig>(() => ({
    ...blankConnection(preset?.engine ?? initial?.engine),
    ...initial,
    ...preset,
  }));
  const [flavor, setFlavor] = useState<EngineChoice>(c.engine);
  const [password, setPassword] = useState(presetPassword ?? '');
  const [clearPassword, setClearPassword] = useState(false);
  const [sshPassword, setSshPassword] = useState('');
  const [clearSshPassword, setClearSshPassword] = useState(false);
  const [sshOpen, setSshOpen] = useState(c.sshEnabled);
  const [sentinelPassword, setSentinelPassword] = useState('');
  const [url, setUrl] = useState('');
  const [testing, setTesting] = useState(false);
  const [test, setTest] = useState<{ ok: boolean; message: string } | null>(null);
  const [saving, setSaving] = useState(false);
  const isEdit = !!initial?.id;
  const file = isFileEngine(c.engine);

  const set = (patch: Partial<ConnectionConfig>) => {
    setC((prev) => ({ ...prev, ...patch }));
    setTest(null);
  };
  const opts: ConnOptions = c.options ?? {};
  const setOpt = (patch: Partial<ConnOptions>) => set({ options: { ...opts, ...patch } });
  const mssqlAuth: MssqlAuth = (c.engine === 'mssql' && opts.auth) || 'sql';
  const redisMode: RedisMode = (c.engine === 'redis' && opts.redisMode) || 'standalone';
  // Sign-in methods that use the User and Password fields.
  const usesPassword = !(c.engine === 'mssql' && (mssqlAuth === 'integrated' || mssqlAuth === 'entra-cli'));
  const sshAllowed = !(c.engine === 'redis' && redisMode !== 'standalone');

  const pickRedisMode = (mode: RedisMode) => {
    const port = c.port || 0;
    setOpt({ redisMode: mode });
    setC((prev) => ({
      ...prev,
      options: { ...(prev.options ?? {}), redisMode: mode },
      // Sentinels listen on 26379 by default.
      port: mode === 'sentinel' && (port === 6379 || !port) ? 26379 : mode !== 'sentinel' && port === 26379 ? 6379 : port,
      database: mode === 'cluster' ? '' : prev.database,
      sshEnabled: mode === 'standalone' ? prev.sshEnabled : false,
    }));
  };

  const pickEngine = (f: EngineChoice) => {
    const engine: Engine = f === 'mariadb' ? 'mysql' : f;
    setFlavor(f);
    const base = blankConnection(engine);
    const sameFamily = engine === c.engine;
    set({
      engine,
      port: c.port === defaultPort(c.engine) || !c.port ? base.port : c.port,
      username: sameFamily ? c.username : base.username,
      database: sameFamily ? c.database : base.database,
      host: isFileEngine(engine) ? '' : c.host || '127.0.0.1',
      filePath: isFileEngine(engine) && isFileEngine(c.engine) && sameFamily ? c.filePath : isFileEngine(engine) ? '' : c.filePath,
      color: c.color === ENGINE_COLOR[c.engine] ? base.color : c.color,
    });
  };

  const applyUrl = (value: string) => {
    setUrl(value);
    const parsed = parseConnectionUrl(value);
    if (!parsed) return;
    const engine = parsed.patch.engine ?? c.engine;
    setFlavor(/^mariadb:/i.test(value) ? 'mariadb' : engine);
    set({
      ...parsed.patch,
      port: parsed.patch.port ?? defaultPort(engine),
      color: ENGINE_COLOR[engine],
      name: c.name || (parsed.patch.database ?? parsed.patch.filePath?.split('/').pop() ?? ''),
    });
    if (parsed.password) setPassword(parsed.password);
  };

  const effectiveName = () => {
    if (c.name.trim()) return c.name.trim();
    if (c.engine === 'redis' && redisMode === 'cluster') return `Redis Cluster @ ${c.host || '127.0.0.1'}`;
    if (c.engine === 'redis' && redisMode === 'sentinel') return `Redis ${opts.sentinelMaster?.trim() || 'mymaster'} (Sentinel)`;
    if (file) return c.filePath === ':memory:' ? 'DuckDB (in memory)' : (c.filePath.split('/').pop() ?? choiceLabel(flavor));
    const remote = c.host && !['127.0.0.1', 'localhost'].includes(c.host) ? ` @ ${c.host.replace(/^mongodb(\+srv)?:\/\//, '')}` : '';
    return `${c.database || choiceLabel(flavor)}${remote}`;
  };

  const sshParams = () => ({
    sshPassword: sshPassword || (isEdit && !clearSshPassword ? null : ''),
    sentinelPassword: sentinelPassword || null,
  });

  const runTest = async () => {
    setTesting(true);
    setTest(null);
    const attempt = () =>
      call<ServerInfo>('connections.test', {
        connection: { ...c, name: effectiveName() },
        password: password || (isEdit && !clearPassword ? null : ''),
        ...sshParams(),
      });
    try {
      let info: ServerInfo;
      try {
        info = await attempt();
      } catch (e) {
        if (!(await trustSsh(e, c))) throw e;
        info = await attempt();
      }
      setTest({
        ok: true,
        message: `Connected · ${info.flavor} ${info.version}${c.sshEnabled ? ` · through ${c.sshHost}` : ''}`,
      });
    } catch (e) {
      setTest({ ok: false, message: errorMessage(e) });
    } finally {
      setTesting(false);
    }
  };

  const save = async (andConnect: boolean) => {
    setSaving(true);
    try {
      const saved = await saveConnection(
        { ...c, name: effectiveName() },
        usesPassword ? password || undefined : undefined,
        clearPassword || !usesPassword,
        {
          sshPassword: sshPassword || undefined,
          clearSshPassword,
          sentinelPassword: sentinelPassword || undefined,
        },
      );
      openModal(null);
      toast('success', isEdit ? 'Connection updated' : 'Connection saved', saved.name);
      if (andConnect) await connect(saved.id);
    } catch (e) {
      if (!handleLimitError(e)) setTest({ ok: false, message: errorMessage(e) });
    } finally {
      setSaving(false);
    }
  };

  const fileFilters =
    c.engine === 'duckdb'
      ? [
          { name: 'DuckDB', extensions: ['duckdb', 'ddb', 'db'] },
          { name: 'All files', extensions: ['*'] },
        ]
      : [
          {
            name: 'SQLite',
            extensions: ['sqlite', 'sqlite3', 'db', 'db3', 's3db', 'sl3'],
          },
          { name: 'All files', extensions: ['*'] },
        ];

  return (
    <Modal
      title={isEdit ? `Edit ${initial?.name}` : 'New connection'}
      onClose={() => openModal(null)}
      width={620}
      footer={
        <>
          <Button variant="ghost" onClick={runTest} disabled={testing}>
            {testing ? <Spinner size={12} /> : null} Test
          </Button>
          <div className="toolbar-spacer" />
          <Button onClick={() => save(false)} disabled={saving}>
            Save
          </Button>
          <Button variant="primary" onClick={() => save(true)} disabled={saving}>
            {saving ? <Spinner size={12} /> : null} {isEdit ? 'Save & connect' : 'Connect'}
          </Button>
        </>
      }
    >
      <form
        className="conn-form"
        onSubmit={(e) => {
          e.preventDefault();
          void save(true);
        }}
      >
        <div className="engine-picker">
          {CHOICES.map((f) => (
            <button type="button" key={f} className={clsx('engine-choice', flavor === f && 'is-active')} onClick={() => pickEngine(f)}>
              <EngineIcon engine={f === 'mariadb' ? 'mysql' : f} flavor={f === 'mariadb' ? 'MariaDB' : undefined} size={26} />
              <span>{choiceLabel(f)}</span>
            </button>
          ))}
        </div>

        {!isEdit && (
          <div className="url-input">
            <Link2 size={13} />
            <input
              className="input"
              value={url}
              placeholder="Paste a connection URL — postgres://, mysql://, sqlserver://, redis://, mongodb://"
              spellCheck={false}
              onChange={(e) => applyUrl(e.target.value)}
            />
          </div>
        )}

        <div className="form-grid">
          <Field label="Name" className="span-2">
            <input className="input" value={c.name} placeholder={effectiveName()} onChange={(e) => set({ name: e.target.value })} autoFocus />
          </Field>
          <Field label="Color">
            <div className="swatches">
              {COLORS.map((col) => (
                <button
                  type="button"
                  key={col}
                  className={clsx('swatch', c.color === col && 'is-active')}
                  style={{ background: col }}
                  onClick={() => set({ color: col })}
                  aria-label={`Color ${col}`}
                />
              ))}
            </div>
          </Field>
        </div>

        <Field
          label="Environment"
          hint={c.env === 'production' ? 'Production connections show a red stripe and ask before running writes.' : undefined}
        >
          <Segmented
            value={c.env as string}
            onChange={(env) => set({ env })}
            options={[
              { value: 'local', label: 'Local' },
              { value: 'development', label: 'Development' },
              { value: 'staging', label: 'Staging' },
              { value: 'production', label: 'Production' },
            ]}
          />
        </Field>

        {file ? (
          <Field
            label="Database file"
            hint={
              c.engine === 'duckdb' ? (
                <>
                  DuckDB can also query CSV, Parquet and JSON files directly ·{' '}
                  <button
                    type="button"
                    className="link-btn"
                    onClick={() =>
                      set({
                        filePath: ':memory:',
                        name: c.name || 'DuckDB (in memory)',
                      })
                    }
                  >
                    use an in-memory database
                  </button>
                </>
              ) : undefined
            }
          >
            <div className="file-row">
              <input
                className="input mono"
                value={c.filePath}
                placeholder={c.engine === 'duckdb' ? '/path/to/analytics.duckdb' : '/path/to/database.sqlite'}
                onChange={(e) => set({ filePath: e.target.value })}
                spellCheck={false}
              />
              <Button
                onClick={async () => {
                  const p = await pickOpenFile(`Open ${ENGINE_LABELS[c.engine]} database`, fileFilters);
                  if (p)
                    set({
                      filePath: p,
                      name: c.name || p.split('/').pop() || '',
                    });
                }}
              >
                <FolderOpen size={13} /> Browse…
              </Button>
              <Button
                title="Create a new, empty database file"
                onClick={async () => {
                  const ext = c.engine === 'duckdb' ? 'duckdb' : 'sqlite';
                  const p = await pickSaveFile(`Create ${ENGINE_LABELS[c.engine]} database`, `database.${ext}`, fileFilters.slice(0, 1));
                  if (!p) return;
                  try {
                    await call('connections.createFile', {
                      engine: c.engine,
                      path: p,
                    });
                    set({
                      filePath: p,
                      name: c.name || p.split('/').pop() || '',
                    });
                  } catch (e) {
                    setTest({ ok: false, message: errorMessage(e) });
                  }
                }}
              >
                <FilePlus2 size={13} /> New…
              </Button>
            </div>
          </Field>
        ) : (
          <>
            {c.engine === 'redis' && (
              <Field
                label="Topology"
                hint={
                  redisMode === 'cluster'
                    ? 'Enter any node; Rowglow discovers the rest and routes each key to its node.'
                    : redisMode === 'sentinel'
                      ? 'Enter a Sentinel; it tells Rowglow where the current master is, even after a failover.'
                      : undefined
                }
              >
                <Segmented
                  value={redisMode}
                  onChange={(m) => pickRedisMode(m as RedisMode)}
                  options={[
                    { value: 'standalone', label: 'Single server' },
                    { value: 'cluster', label: 'Cluster' },
                    { value: 'sentinel', label: 'Sentinel' },
                  ]}
                />
              </Field>
            )}
            {c.engine === 'mssql' && (
              <Field
                label="Sign in with"
                hint={
                  mssqlAuth === 'entra-cli'
                    ? 'Uses the account you signed in to with az login (MFA works). Rowglow asks the Azure CLI for a token when it connects.'
                    : mssqlAuth === 'entra-sp'
                      ? 'An app registration with access to the database. The client secret is stored in your system keychain.'
                      : mssqlAuth === 'integrated'
                        ? 'Signs in as the Windows user running Rowglow (domain or local account).'
                        : !IS_WINDOWS
                          ? 'Windows sign-in is available in Rowglow for Windows.'
                          : undefined
                }
              >
                <select
                  className="select"
                  value={mssqlAuth}
                  onChange={(e) => {
                    const auth = e.target.value as MssqlAuth;
                    setC((prev) => ({
                      ...prev,
                      options: { ...(prev.options ?? {}), auth },
                      username: auth === 'sql' && !prev.username ? 'sa' : auth !== 'sql' && prev.username === 'sa' ? '' : prev.username,
                      // Azure SQL always encrypts.
                      sslMode: auth.startsWith('entra') && prev.sslMode === 'disable' ? 'require' : prev.sslMode,
                    }));
                    setTest(null);
                  }}
                >
                  {MSSQL_AUTH.filter((a) => !a.windowsOnly || IS_WINDOWS || a.value === mssqlAuth).map((a) => (
                    <option key={a.value} value={a.value}>
                      {a.label}
                      {a.windowsOnly && !IS_WINDOWS ? ' (Windows only)' : ''}
                    </option>
                  ))}
                </select>
              </Field>
            )}
            <div className="form-grid">
              <Field
                label={
                  c.engine === 'redis' && redisMode === 'sentinel'
                    ? 'Sentinel host'
                    : c.engine === 'redis' && redisMode === 'cluster'
                      ? 'Node host'
                      : 'Host'
                }
                className="span-2"
                hint={
                  c.engine === 'postgres' || c.engine === 'mysql'
                    ? 'A path such as /tmp uses a Unix socket'
                    : c.engine === 'mongodb'
                      ? 'Or paste a full mongodb+srv:// address (Atlas)'
                      : c.engine === 'mssql' && mssqlAuth.startsWith('entra')
                        ? 'e.g. myserver.database.windows.net'
                        : undefined
                }
              >
                <input
                  className="input mono"
                  value={c.host}
                  placeholder="127.0.0.1"
                  onChange={(e) => set({ host: e.target.value })}
                  spellCheck={false}
                />
              </Field>
              <Field label="Port">
                <input
                  className="input mono"
                  inputMode="numeric"
                  value={c.port || ''}
                  placeholder={c.engine === 'redis' && redisMode === 'sentinel' ? '26379' : String(defaultPort(c.engine))}
                  onChange={(e) =>
                    set({
                      port: Number(e.target.value.replace(/\D/g, '')) || 0,
                    })
                  }
                />
              </Field>
            </div>
            {c.engine === 'redis' && redisMode !== 'standalone' && (
              <div className="form-grid">
                <Field
                  label={redisMode === 'cluster' ? 'More nodes' : 'More Sentinels'}
                  className={redisMode === 'cluster' ? 'span-3' : 'span-2'}
                  hint={redisMode === 'cluster' ? 'Optional, used if the node above is down' : 'Optional, used if the Sentinel above is down'}
                >
                  <input
                    className="input mono"
                    value={opts.redisNodes ?? ''}
                    placeholder={redisMode === 'cluster' ? '10.0.0.2:6379, 10.0.0.3:6379' : '10.0.0.2:26379, 10.0.0.3:26379'}
                    onChange={(e) => setOpt({ redisNodes: e.target.value })}
                    spellCheck={false}
                  />
                </Field>
                {redisMode === 'sentinel' && (
                  <Field label="Master name">
                    <input
                      className="input mono"
                      value={opts.sentinelMaster ?? ''}
                      placeholder="mymaster"
                      onChange={(e) => setOpt({ sentinelMaster: e.target.value })}
                      spellCheck={false}
                    />
                  </Field>
                )}
              </div>
            )}
            {c.engine === 'redis' && redisMode === 'sentinel' && (
              <div className="form-grid">
                <Field label="Connect to" hint={opts.redisReplica ? 'Replicas are read-only, so the connection is too' : undefined}>
                  <Segmented
                    value={opts.redisReplica ? 'replica' : 'master'}
                    onChange={(v) => setOpt({ redisReplica: v === 'replica' })}
                    options={[
                      { value: 'master', label: 'Master' },
                      { value: 'replica', label: 'Replica' },
                    ]}
                  />
                </Field>
                <Field label="Sentinel user">
                  <input
                    className="input mono"
                    value={opts.sentinelUser ?? ''}
                    placeholder="optional"
                    onChange={(e) => setOpt({ sentinelUser: e.target.value })}
                    spellCheck={false}
                    autoComplete="off"
                  />
                </Field>
                <Field label="Sentinel password" hint="Only if the Sentinels have their own password">
                  <input
                    className="input mono"
                    type="password"
                    value={sentinelPassword}
                    placeholder={isEdit && opts.redisMode === 'sentinel' && initial?.options?.redisMode === 'sentinel' ? 'unchanged' : ''}
                    onChange={(e) => setSentinelPassword(e.target.value)}
                    autoComplete="new-password"
                  />
                </Field>
              </div>
            )}
            {c.engine === 'mssql' && mssqlAuth.startsWith('entra') && (
              <div className="form-grid">
                <Field
                  label={mssqlAuth === 'entra-sp' ? 'Tenant' : 'Tenant (optional)'}
                  className="span-3"
                  hint={
                    mssqlAuth === 'entra-cli'
                      ? 'Only if your Azure CLI account belongs to several tenants'
                      : 'Directory (tenant) ID, or a domain like contoso.onmicrosoft.com'
                  }
                >
                  <input
                    className="input mono"
                    value={opts.tenant ?? ''}
                    placeholder="00000000-0000-0000-0000-000000000000"
                    onChange={(e) => setOpt({ tenant: e.target.value.trim() })}
                    spellCheck={false}
                  />
                </Field>
              </div>
            )}
            <div className="form-grid">
              {usesPassword && (
                <Field
                  label={
                    c.engine === 'redis'
                      ? 'User (ACL)'
                      : c.engine === 'mssql' && mssqlAuth === 'entra-sp'
                        ? 'Client ID'
                        : c.engine === 'mssql' && mssqlAuth === 'windows'
                          ? 'Windows user'
                          : 'User'
                  }
                >
                  <input
                    className="input mono"
                    value={c.username}
                    placeholder={
                      c.engine === 'mssql' && mssqlAuth === 'entra-sp'
                        ? 'application (client) ID'
                        : c.engine === 'mssql' && mssqlAuth === 'windows'
                          ? 'CORP\\jane'
                          : userPlaceholder(c.engine)
                    }
                    onChange={(e) => set({ username: e.target.value })}
                    spellCheck={false}
                    autoComplete="off"
                  />
                </Field>
              )}
              {usesPassword && (
                <Field
                  label={c.engine === 'mssql' && mssqlAuth === 'entra-sp' ? 'Client secret' : 'Password'}
                  hint={
                    isEdit && initial?.hasPassword && !password && !clearPassword ? (
                      <>
                        Saved in {keychainName} ·{' '}
                        <button type="button" className="link-btn" onClick={() => setClearPassword(true)}>
                          remove
                        </button>
                      </>
                    ) : clearPassword ? (
                      'Saved password will be removed'
                    ) : (
                      'Stored in your system keychain'
                    )
                  }
                >
                  <input
                    className="input mono"
                    type="password"
                    value={password}
                    placeholder={isEdit && initial?.hasPassword && !clearPassword ? '••••••••' : ''}
                    onChange={(e) => {
                      setPassword(e.target.value);
                      setClearPassword(false);
                    }}
                    autoComplete="new-password"
                  />
                </Field>
              )}
              {!(c.engine === 'redis' && redisMode === 'cluster') && (
                <Field
                  label={c.engine === 'redis' ? 'DB index' : 'Database'}
                  className={usesPassword ? undefined : 'span-3'}
                  hint={c.engine === 'mongodb' ? 'Users are checked against admin' : undefined}
                >
                  <input
                    className="input mono"
                    value={c.database}
                    inputMode={c.engine === 'redis' ? 'numeric' : undefined}
                    placeholder={c.engine === 'postgres' ? 'postgres' : c.engine === 'redis' ? '0' : c.engine === 'mssql' ? 'master' : 'optional'}
                    onChange={(e) =>
                      set({
                        database: c.engine === 'redis' ? e.target.value.replace(/\D/g, '') : e.target.value,
                      })
                    }
                    spellCheck={false}
                  />
                </Field>
              )}
            </div>
            <div className="form-grid">
              <Field label={c.engine === 'redis' || c.engine === 'mongodb' ? 'TLS' : 'SSL'}>
                <select
                  className="select"
                  value={c.sslMode}
                  onChange={(e) =>
                    set({
                      sslMode: e.target.value as ConnectionConfig['sslMode'],
                    })
                  }
                >
                  <option value="disable">Disable</option>
                  <option value="prefer">{c.engine === 'redis' || c.engine === 'mongodb' ? 'Off' : 'Prefer'}</option>
                  <option value="require">Require</option>
                </select>
              </Field>
              <Field label="Group" className="span-2">
                <input
                  className="input"
                  value={c.groupName}
                  placeholder="Optional, e.g. Client A"
                  onChange={(e) => set({ groupName: e.target.value })}
                />
              </Field>
            </div>

            {sshAllowed && (
              <div className="ssh-section">
                <button type="button" className="ssh-toggle" onClick={() => setSshOpen((v) => !v)}>
                  {sshOpen ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
                  <KeyRound size={13} />
                  <span>SSH tunnel</span>
                  {c.sshEnabled && <span className="tag">on · {c.sshHost || 'host?'}</span>}
                </button>
                {sshOpen && (
                  <div className="ssh-body">
                    <Toggle
                      checked={c.sshEnabled}
                      onChange={(sshEnabled) => set({ sshEnabled })}
                      label="Connect through an SSH server (bastion). The host above is resolved from that server."
                    />
                    {c.sshEnabled && (
                      <>
                        <div className="form-grid">
                          <Field label="SSH host" className="span-2">
                            <input
                              className="input mono"
                              value={c.sshHost}
                              placeholder="bastion.example.com"
                              onChange={(e) => set({ sshHost: e.target.value })}
                              spellCheck={false}
                            />
                          </Field>
                          <Field label="Port">
                            <input
                              className="input mono"
                              inputMode="numeric"
                              value={c.sshPort || ''}
                              placeholder="22"
                              onChange={(e) =>
                                set({
                                  sshPort: Number(e.target.value.replace(/\D/g, '')) || 0,
                                })
                              }
                            />
                          </Field>
                        </div>
                        <div className="form-grid">
                          <Field label="SSH user">
                            <input
                              className="input mono"
                              value={c.sshUser}
                              placeholder="ubuntu"
                              onChange={(e) => set({ sshUser: e.target.value })}
                              spellCheck={false}
                              autoComplete="off"
                            />
                          </Field>
                          <Field label="Sign in with" className="span-2">
                            <Segmented
                              value={c.sshAuth === 'password' ? 'password' : 'key'}
                              onChange={(sshAuth) => set({ sshAuth })}
                              options={[
                                { value: 'key', label: 'Private key' },
                                { value: 'password', label: 'Password' },
                              ]}
                            />
                          </Field>
                        </div>
                        {c.sshAuth !== 'password' && (
                          <Field label="Private key" hint="Leave empty to use ~/.ssh/id_ed25519, id_ecdsa or id_rsa">
                            <div className="file-row">
                              <input
                                className="input mono"
                                value={c.sshKeyPath}
                                placeholder="~/.ssh/id_ed25519"
                                onChange={(e) => set({ sshKeyPath: e.target.value })}
                                spellCheck={false}
                              />
                              <Button
                                onClick={async () => {
                                  const p = await pickOpenFile('Choose SSH private key');
                                  if (p) set({ sshKeyPath: p });
                                }}
                              >
                                <FolderOpen size={13} /> Browse…
                              </Button>
                            </div>
                          </Field>
                        )}
                        <Field
                          label={c.sshAuth === 'password' ? 'SSH password' : 'Key passphrase'}
                          hint={
                            isEdit && initial?.hasSshSecret && !sshPassword && !clearSshPassword ? (
                              <>
                                Saved in {keychainName} ·{' '}
                                <button type="button" className="link-btn" onClick={() => setClearSshPassword(true)}>
                                  remove
                                </button>
                              </>
                            ) : c.sshAuth === 'password' ? (
                              'Stored in your system keychain'
                            ) : (
                              'Only if the key is encrypted'
                            )
                          }
                        >
                          <input
                            className="input mono"
                            type="password"
                            value={sshPassword}
                            placeholder={isEdit && initial?.hasSshSecret && !clearSshPassword ? '••••••••' : ''}
                            onChange={(e) => {
                              setSshPassword(e.target.value);
                              setClearSshPassword(false);
                            }}
                            autoComplete="new-password"
                          />
                        </Field>
                      </>
                    )}
                  </div>
                )}
              </div>
            )}
          </>
        )}

        <div className="form-toggles">
          <Toggle
            checked={c.readOnly}
            onChange={(readOnly) => set({ readOnly })}
            label={
              c.engine === 'redis' || c.engine === 'mongodb'
                ? 'Read-only — Rowglow blocks every command that writes'
                : 'Read-only — block every write at the database session level'
            }
          />
        </div>

        {test && (
          <div className={clsx('callout', test.ok ? 'callout-ok' : 'callout-error')}>
            {test.ok ? <CheckCircle2 size={14} /> : <XCircle size={14} />}
            <span>{test.message}</span>
          </div>
        )}
        <button type="submit" hidden />
      </form>
    </Modal>
  );
}
