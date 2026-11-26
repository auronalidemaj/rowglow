import clsx from 'clsx';
import { isFileEngine } from '../lib/types';
import { Home as HomeIcon, PanelLeft, PanelRight, Plus, Search, Settings, TerminalSquare, Unplug, Pencil, ArrowUpCircle, Sparkles } from 'lucide-react';
import { isMac, isTauri, call } from '../lib/rpc';
import { formatDayUtc, initials } from '../lib/format';
import { mod } from '../lib/platform';
import type { ConnectionConfig } from '../lib/types';
import { activeWorkspace, useStore } from '../state/store';
import { EnvBadge, IconButton, Kbd, LogoMark, useMenu } from './ui';

// macOS hides the native title bar (tauri.conf.json `titleBarStyle: Overlay`),
// so this header *is* the title bar: it leaves room for the traffic lights and
// drags the window. Windows/Linux keep native decorations, so there it's a
// plain toolbar under the system title bar and menu.
const macChrome = isTauri && isMac;
const drag = macChrome ? { 'data-tauri-drag-region': true } : {};

export function TitleBar() {
  const ws = useStore(activeWorkspace);
  const sidebarVisible = useStore((s) => s.sidebarVisible);
  const inspectorVisible = useStore((s) => s.inspectorVisible);
  const { setPalette, toggleSidebar, toggleInspector, openQuery, openModal } = useStore.getState();
  const prod = ws?.connection.env === 'production';
  return (
    <header className={clsx('titlebar', prod && 'is-production')} {...drag}>
      <div className={clsx('titlebar-left', macChrome && 'has-traffic-lights')} {...drag}>
        {ws ? (
          <div className="crumbs" {...drag}>
            <span className="crumb-dot" style={{ background: ws.connection.color || 'var(--accent)' }} />
            <span className="crumb-name">{ws.connection.name}</span>
            <EnvBadge env={ws.connection.env} compact />
            <span className="crumb-sep">/</span>
            <span className="crumb-dim">{ws.info.database}</span>
            {(ws.connection.engine === 'postgres' || ws.connection.engine === 'mssql' || (ws.connection.engine === 'duckdb' && ws.schema !== 'main')) && (
              <>
                <span className="crumb-sep">/</span>
                <span className="crumb-dim">{ws.schema}</span>
              </>
            )}
            {ws.info.readOnly && <span className="tag">read-only</span>}
          </div>
        ) : (
          <div className="crumbs" {...drag}>
            <LogoMark size={16} />
            <span className="crumb-name">Rowglow</span>
          </div>
        )}
      </div>
      <button className="omnibox" onClick={() => setPalette(true)}>
        <Search size={13} />
        <span>{ws ? `Search tables, queries and commands` : 'Search connections and commands'}</span>
        <Kbd>{mod}K</Kbd>
      </button>
      <div className="titlebar-right" {...drag}>
        {ws && (
          <>
            <IconButton label={`New query (${mod}T)`} onClick={() => openQuery(ws.sessionId)}>
              <TerminalSquare size={15} />
            </IconButton>
            <IconButton label={`Toggle sidebar (${mod}\\)`} active={sidebarVisible} onClick={toggleSidebar}>
              <PanelLeft size={15} />
            </IconButton>
            <IconButton label={`Toggle inspector (${mod}⇧I)`} active={inspectorVisible} onClick={toggleInspector}>
              <PanelRight size={15} />
            </IconButton>
          </>
        )}
        <IconButton label={`Settings (${mod},)`} onClick={() => openModal({ kind: 'settings' })}>
          <Settings size={15} />
        </IconButton>
      </div>
    </header>
  );
}

export function Rail() {
  const workspaces = useStore((s) => s.workspaces);
  const active = useStore((s) => s.active);
  const { setActive, disconnect, openModal, openQuery } = useStore.getState();
  const [menu, openMenu] = useMenu();

  const edit = async (c: ConnectionConfig) => {
    const full = await call<ConnectionConfig>('connections.get', { id: c.id });
    openModal({ kind: 'connection', connection: full });
  };

  return (
    <nav className="rail">
      <button className={clsx('rail-item rail-home', active === null && 'is-active')} onClick={() => setActive(null)} title={`Connections (${mod}0)`}>
        <HomeIcon size={17} />
      </button>
      <div className="rail-sep" />
      <div className="rail-list">
        {workspaces.map((w, i) => (
          <button
            key={w.sessionId}
            className={clsx('rail-item rail-conn', w.sessionId === active && 'is-active', w.connection.env === 'production' && 'is-production')}
            style={{ ['--conn-color' as string]: w.connection.color || 'var(--accent)' }}
            onClick={() => setActive(w.sessionId)}
            title={`${w.connection.name} · ${w.info.database}${i < 9 ? `  (${mod}${i + 1})` : ''}`}
            onContextMenu={(e) =>
              openMenu(e, [
                { label: 'New query', icon: <TerminalSquare size={14} />, onSelect: () => openQuery(w.sessionId) },
                { label: 'Edit connection…', icon: <Pencil size={14} />, onSelect: () => edit(w.connection) },
                { separator: true },
                { label: 'Disconnect', icon: <Unplug size={14} />, danger: true, onSelect: () => disconnect(w.sessionId) },
              ])
            }
          >
            <span className="rail-avatar">{initials(w.connection.name)}</span>
            {w.tabs.some((t) => t.kind === 'table' && t.dirty) && <span className="rail-dirty" />}
          </button>
        ))}
      </div>
      <button className="rail-item rail-add" onClick={() => openModal({ kind: 'connection' })} title={`New connection (${mod}N)`}>
        <Plus size={17} />
      </button>
      {menu}
    </nav>
  );
}

export function StatusBar() {
  const ws = useStore(activeWorkspace);
  const license = useStore((s) => s.license);
  const update = useStore((s) => s.update);
  const appInfo = useStore((s) => s.appInfo);
  const openModal = useStore((s) => s.openModal);
  return (
    <footer className="statusbar">
      <div className="status-left">
        {ws ? (
          <>
            <span className="status-dot" />
            <span>
              {ws.info.flavor} {ws.info.version}
            </span>
            {!isFileEngine(ws.connection.engine) && (
              <span className="dim">
                {ws.info.user.split('@')[0]}@{ws.connection.host || 'localhost'}
                {ws.connection.port ? `:${ws.connection.port}` : ''}
              </span>
            )}
            {ws.connection.sshEnabled && !isFileEngine(ws.connection.engine) && <span className="dim">via SSH {ws.connection.sshHost}</span>}
            {isFileEngine(ws.connection.engine) && <span className="dim mono">{ws.connection.filePath}</span>}
          </>
        ) : (
          <span className="dim">Not connected</span>
        )}
      </div>
      <div className="status-right">
        {update?.updateAvailable && (
          <button className="status-chip is-accent" onClick={() => openModal({ kind: 'settings', section: 'about' })}>
            <ArrowUpCircle size={11} /> Update {update.version}
          </button>
        )}
        {license && (
          <button className={clsx('status-chip', (license.licensed || license.freeUntil) && 'is-pro')} onClick={() => openModal({ kind: 'settings', section: 'license' })}>
            {license.licensed ? (
              <>
                <Sparkles size={11} /> {license.plan === 'team' ? 'Team' : 'Pro'}
              </>
            ) : license.freeUntil ? (
              <>
                <Sparkles size={11} /> Free until {formatDayUtc(license.freeUntil)}
              </>
            ) : (
              'Free · Upgrade'
            )}
          </button>
        )}
        {appInfo && <span className="dim">v{appInfo.version}</span>}
      </div>
    </footer>
  );
}
