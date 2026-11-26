import clsx from 'clsx';
import { Code2, Plus, Table2, TerminalSquare, X, Eye, Star, Network } from 'lucide-react';
import { useRef } from 'react';
import { mod } from '../lib/platform';
import { useStore, type Tab, type Workspace } from '../state/store';
import { IconButton, useMenu } from './ui';

function tabTitle(t: Tab): string {
  if (t.kind === 'query') return t.title;
  return t.name;
}

function TabIcon({ t }: { t: Tab }) {
  if (t.kind === 'query') return t.snippetId ? <Star size={12} /> : <TerminalSquare size={12} />;
  if (t.kind === 'object') return <Code2 size={12} />;
  if (t.kind === 'diagram') return <Network size={12} />;
  return t.objectKind === 'view' || t.objectKind === 'matview' ? <Eye size={12} /> : <Table2 size={12} />;
}

export function TabBar({ ws }: { ws: Workspace }) {
  const { setActiveTab, closeTab, openQuery, updateTab, updateWorkspace } = useStore.getState();
  const [menu, openMenu] = useMenu();
  const drag = useRef<string | null>(null);

  const closeOthers = async (keep: string) => {
    for (const t of ws.tabs) if (t.id !== keep) await closeTab(ws.sessionId, t.id);
  };

  return (
    <div className="tabbar" onDoubleClick={(e) => e.target === e.currentTarget && openQuery(ws.sessionId)}>
      <div className="tabbar-scroll">
        {ws.tabs.map((t) => (
          <div
            key={t.id}
            className={clsx('tab', t.id === ws.activeTabId && 'is-active', t.kind === 'table' && t.preview && 'is-preview')}
            onMouseDown={(e) => {
              if (e.button === 1) {
                e.preventDefault();
                void closeTab(ws.sessionId, t.id);
              } else if (e.button === 0) setActiveTab(ws.sessionId, t.id);
            }}
            onDoubleClick={() => t.kind === 'table' && updateTab(ws.sessionId, t.id, { preview: false })}
            onContextMenu={(e) =>
              openMenu(e, [
                { label: 'Close', shortcut: `${mod}W`, onSelect: () => closeTab(ws.sessionId, t.id) },
                { label: 'Close others', onSelect: () => closeOthers(t.id) },
                { label: 'Close all', onSelect: () => closeOthers('') },
                ...(t.kind === 'table' && t.preview ? [{ separator: true }, { label: 'Keep open', onSelect: () => updateTab(ws.sessionId, t.id, { preview: false }) }] : []),
              ])
            }
            draggable
            onDragStart={() => (drag.current = t.id)}
            onDragOver={(e) => e.preventDefault()}
            onDrop={() => {
              const from = drag.current;
              drag.current = null;
              if (!from || from === t.id) return;
              const tabs = [...ws.tabs];
              const fi = tabs.findIndex((x) => x.id === from);
              const ti = tabs.findIndex((x) => x.id === t.id);
              const [moved] = tabs.splice(fi, 1);
              tabs.splice(ti, 0, moved);
              updateWorkspace(ws.sessionId, { tabs });
            }}
            title={t.kind === 'table' ? `${t.schema}.${t.name}` : tabTitle(t)}
          >
            <span className="tab-icon">
              <TabIcon t={t} />
            </span>
            <span className="tab-title">{tabTitle(t)}</span>
            {(t.kind === 'table' || t.kind === 'query') && t.dirty ? (
              <span className="tab-dirty" onMouseDown={(e) => { e.stopPropagation(); void closeTab(ws.sessionId, t.id); }} />
            ) : (
              <button
                className="tab-close"
                aria-label="Close tab"
                onMouseDown={(e) => {
                  e.stopPropagation();
                  void closeTab(ws.sessionId, t.id);
                }}
              >
                <X size={11} />
              </button>
            )}
          </div>
        ))}
      </div>
      <IconButton label={`New query (${mod}T)`} onClick={() => openQuery(ws.sessionId)} className="tabbar-add">
        <Plus size={15} />
      </IconButton>
      {menu}
    </div>
  );
}
