import clsx from 'clsx';
import {
  Code2,
  Command,
  Database,
  Eye,
  FolderOpen,
  FunctionSquare,
  Keyboard,
  Moon,
  Plus,
  Radar,
  RefreshCw,
  Settings,
  Star,
  Table2,
  TerminalSquare,
  Unplug,
  Home,
  PanelLeft,
  PanelRight,
  Network,
  FileUp,
  Sparkles,
} from 'lucide-react';
import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { fuzzyScore } from '../lib/fuzzy';
import { mod } from '../lib/platform';
import { isSqlEngine } from '../lib/types';
import { activeWorkspace, useStore } from '../state/store';
import { EngineIcon, Kbd } from './ui';

interface Item {
  id: string;
  group: string;
  label: string;
  hint?: string;
  icon: ReactNode;
  shortcut?: string;
  run(): void;
}

export function CommandPalette({ onOpenSqlFile }: { onOpenSqlFile(): void }) {
  const open = useStore((s) => s.paletteOpen);
  const ws = useStore(activeWorkspace);
  const connections = useStore((s) => s.connections);
  const snippets = useStore((s) => s.snippets);
  const settings = useStore((s) => s.settings);
  const st = useStore.getState();
  const [q, setQ] = useState('');
  const [idx, setIdx] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (open) {
      setQ('');
      setIdx(0);
    }
  }, [open]);

  const items = useMemo<Item[]>(() => {
    if (!open) return [];
    const out: Item[] = [];
    if (ws) {
      for (const o of ws.objects) {
        const isRel = ['table', 'view', 'matview', 'foreign'].includes(o.kind);
        out.push({
          id: `obj:${o.kind}:${o.name}:${o.signature ?? ''}`,
          group: isRel ? 'Tables & views' : 'Routines & types',
          label: o.name,
          hint: o.kind === 'table' ? ws.schema : `${o.kind} · ${ws.schema}`,
          icon:
            o.kind === 'view' || o.kind === 'matview' ? (
              <Eye size={14} />
            ) : isRel ? (
              <Table2 size={14} />
            ) : o.kind === 'function' || o.kind === 'procedure' ? (
              <FunctionSquare size={14} />
            ) : (
              <Code2 size={14} />
            ),
          run: () => (isRel ? st.openTable(ws.sessionId, ws.schema, o) : st.openObject(ws.sessionId, ws.schema, o)),
        });
      }
      for (const s of snippets.filter((s) => !s.connectionId || s.connectionId === ws.connection.id)) {
        out.push({
          id: `snip:${s.id}`,
          group: 'Saved queries',
          label: s.name,
          hint: s.sql.replace(/\s+/g, ' ').slice(0, 60),
          icon: <Star size={14} />,
          run: () =>
            st.openQuery(ws.sessionId, {
              sql: s.sql,
              title: s.name,
              snippetId: s.id,
            }),
        });
      }
    }
    for (const c of connections) {
      out.push({
        id: `conn:${c.id}`,
        group: 'Connections',
        label: c.name,
        hint:
          c.engine === 'sqlite' || c.engine === 'duckdb'
            ? c.filePath
            : `${c.host || 'localhost'}${c.port ? ':' + c.port : ''}${c.database ? '/' + c.database : ''}`,
        icon: <EngineIcon engine={c.engine} size={18} />,
        run: () => void st.connect(c.id),
      });
    }
    const cmd = (id: string, label: string, icon: ReactNode, run: () => void, shortcut?: string) =>
      out.push({
        id: `cmd:${id}`,
        group: 'Commands',
        label,
        icon,
        run,
        shortcut,
      });
    if (ws) {
      cmd('new-query', 'New query', <TerminalSquare size={14} />, () => st.openQuery(ws.sessionId), `${mod}T`);
      cmd('refresh', 'Refresh schema', <RefreshCw size={14} />, () => {
        void st.loadObjects(ws.sessionId);
        void st.loadCompletions(ws.sessionId);
      });
      cmd('disconnect', `Disconnect ${ws.connection.name}`, <Unplug size={14} />, () => void st.disconnect(ws.sessionId));
      if (isSqlEngine(ws.connection.engine)) {
        cmd('ask-ai', 'Ask AI for a query…', <Sparkles size={14} />, () => st.openQuery(ws.sessionId, { askAi: true }), `${mod}I`);
        cmd('diagram', `ER diagram of ${ws.schema}`, <Network size={14} />, () => st.openDiagram(ws.sessionId));
        if (!ws.info.readOnly)
          cmd('import-csv', 'Import CSV…', <FileUp size={14} />, () => st.openModal({ kind: 'import', sessionId: ws.sessionId }));
      }
      if (ws.connection.engine === 'postgres' || ws.connection.engine === 'mssql')
        for (const d of ws.databases.filter((d) => d !== ws.info.database))
          cmd(`db:${d}`, `Switch database to ${d}`, <Database size={14} />, () => void st.switchDatabase(ws.sessionId, d));
    }
    cmd('home', 'Go to connections', <Home size={14} />, () => st.setActive(null), `${mod}0`);
    cmd('new-conn', 'New connection…', <Plus size={14} />, () => st.openModal({ kind: 'connection' }), `${mod}N`);
    cmd('open-sql', 'Open SQL file…', <FolderOpen size={14} />, onOpenSqlFile, `${mod}O`);
    cmd('radar', 'Scan for local databases', <Radar size={14} />, () => {
      st.setActive(null);
      void st.scanRadar();
    });
    cmd(
      'theme',
      `Switch to ${document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark'} theme`,
      <Moon size={14} />,
      () =>
        void st.setSettings({
          theme: document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark',
        }),
    );
    cmd('sidebar', 'Toggle sidebar', <PanelLeft size={14} />, st.toggleSidebar, `${mod}\\`);
    cmd('inspector', 'Toggle inspector', <PanelRight size={14} />, st.toggleInspector, `⇧${mod}I`);
    cmd('settings', 'Settings', <Settings size={14} />, () => st.openModal({ kind: 'settings' }), `${mod},`);
    cmd('license', 'License & upgrade', <Star size={14} />, () => st.openModal({ kind: 'settings', section: 'license' }));
    cmd('shortcuts', 'Keyboard shortcuts', <Keyboard size={14} />, () => st.openModal({ kind: 'shortcuts' }));
    cmd('updates', 'Check for updates', <RefreshCw size={14} />, () => void st.checkUpdates(true));
    return out;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, ws, connections, snippets, settings.theme]);

  const results = useMemo(() => {
    const query = q.trim();
    if (!query) {
      // Without a query: tables first (if connected), then commands.
      const base = ws ? items.filter((i) => i.group === 'Tables & views').slice(0, 8) : items.filter((i) => i.group === 'Connections').slice(0, 8);
      return [...base, ...items.filter((i) => i.group === 'Commands').slice(0, 8)];
    }
    const commandsOnly = query.startsWith('>');
    const qq = commandsOnly ? query.slice(1).trim() : query;
    return items
      .filter((i) => !commandsOnly || i.group === 'Commands')
      .map((i) => ({
        i,
        s: fuzzyScore(qq, i.label) + (i.group === 'Tables & views' ? 15 : 0),
      }))
      .filter((x) => x.s >= 0)
      .sort((a, b) => b.s - a.s)
      .slice(0, 40)
      .map((x) => x.i);
  }, [items, q, ws]);

  useEffect(() => setIdx(0), [q]);
  useEffect(() => {
    listRef.current?.querySelector('.is-selected')?.scrollIntoView({ block: 'nearest' });
  }, [idx]);

  if (!open) return null;
  const close = () => st.setPalette(false);
  const choose = (i: Item | undefined) => {
    if (!i) return;
    close();
    setTimeout(i.run, 0);
  };

  let lastGroup = '';
  return createPortal(
    <div className="palette-backdrop" onMouseDown={(e) => e.target === e.currentTarget && close()}>
      <div className="palette" role="dialog">
        <div className="palette-input">
          <Command size={15} />
          <input
            autoFocus
            value={q}
            placeholder={ws ? `Jump to a table in ${ws.connection.name}, a saved query or a command…` : 'Search connections and commands…'}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'ArrowDown') {
                e.preventDefault();
                setIdx((i) => Math.min(results.length - 1, i + 1));
              } else if (e.key === 'ArrowUp') {
                e.preventDefault();
                setIdx((i) => Math.max(0, i - 1));
              } else if (e.key === 'Enter') {
                e.preventDefault();
                choose(results[idx]);
              } else if (e.key === 'Escape') {
                e.preventDefault();
                close();
              }
            }}
          />
        </div>
        <div className="palette-list" ref={listRef}>
          {results.length === 0 && <div className="palette-empty">No matches</div>}
          {results.map((r, i) => {
            const header = r.group !== lastGroup ? r.group : null;
            lastGroup = r.group;
            return (
              <div key={r.id}>
                {header && <div className="palette-group">{header}</div>}
                <button className={clsx('palette-item', i === idx && 'is-selected')} onMouseMove={() => setIdx(i)} onClick={() => choose(r)}>
                  <span className="palette-icon">{r.icon}</span>
                  <span className="palette-label">{r.label}</span>
                  {r.hint && <span className="palette-hint">{r.hint}</span>}
                  {r.shortcut && <Kbd>{r.shortcut}</Kbd>}
                </button>
              </div>
            );
          })}
        </div>
        <div className="palette-foot">
          <span>
            <Kbd>↑↓</Kbd> navigate
          </span>
          <span>
            <Kbd>↵</Kbd> open
          </span>
          <span>
            <Kbd>&gt;</Kbd> commands only
          </span>
        </div>
      </div>
    </div>,
    document.body,
  );
}
