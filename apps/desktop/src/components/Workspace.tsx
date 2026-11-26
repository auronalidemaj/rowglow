import { Search, TerminalSquare, Table2 } from 'lucide-react';
import { useState } from 'react';
import { mod } from '../lib/platform';
import { useStore, type Workspace as WorkspaceT } from '../state/store';
import { Inspector } from './Inspector';
import { QueryView } from './QueryView';
import { Sidebar } from './Sidebar';
import { TabBar } from './TabBar';
import { DdlPanel, TableView } from './TableView';
import { ErDiagram } from './ErDiagram';
import { RedisWorkspace } from './RedisWorkspace';
import { MongoWorkspace } from './MongoWorkspace';
import { EngineIcon, EnvBadge, Kbd, Splitter } from './ui';
import { ErrorBoundary } from './ErrorBoundary';

export function WorkspaceView({ ws, visible }: { ws: WorkspaceT; visible: boolean }) {
  if (ws.connection.engine === 'redis') return <RedisWorkspace ws={ws} visible={visible} />;
  if (ws.connection.engine === 'mongodb') return <MongoWorkspace ws={ws} visible={visible} />;
  return <SqlWorkspace ws={ws} visible={visible} />;
}

function SqlWorkspace({ ws, visible }: { ws: WorkspaceT; visible: boolean }) {
  const sidebarVisible = useStore((s) => s.sidebarVisible);
  const inspectorVisible = useStore((s) => s.inspectorVisible);
  const [sidebarW, setSidebarW] = useState(260);
  const [inspectorW, setInspectorW] = useState(320);
  const { openQuery, setPalette, openTable } = useStore.getState();
  const tables = ws.objects.filter((o) => o.kind === 'table').slice(0, 6);

  return (
    <div className="workspace" style={{ display: visible ? 'flex' : 'none' }}>
      {sidebarVisible && (
        <>
          <ErrorBoundary label="the sidebar" compact>
            <Sidebar ws={ws} width={sidebarW} />
          </ErrorBoundary>
          <Splitter direction="vertical" onDrag={(d) => setSidebarW((w) => Math.max(180, Math.min(520, w + d)))} />
        </>
      )}
      <main className="main">
        {ws.tabs.length > 0 && <TabBar ws={ws} />}
        <div className="tab-content">
          {ws.tabs.length === 0 && (
            <div className="ws-welcome">
              <div className="ws-welcome-card">
                <div className="ws-welcome-head">
                  <EngineIcon engine={ws.connection.engine} flavor={ws.info.flavor} size={36} />
                  <div>
                    <div className="ws-welcome-title">
                      {ws.connection.name} <EnvBadge env={ws.connection.env} />
                    </div>
                    <div className="dim">
                      {ws.info.flavor} {ws.info.version} · {ws.info.database}
                      {ws.info.user ? ` · ${ws.info.user}` : ''}
                    </div>
                  </div>
                </div>
                <div className="ws-actions">
                  <button className="ws-action" onClick={() => openQuery(ws.sessionId)}>
                    <TerminalSquare size={16} />
                    <span>New query</span>
                    <Kbd>{mod}T</Kbd>
                  </button>
                  <button className="ws-action" onClick={() => setPalette(true)}>
                    <Search size={16} />
                    <span>Open anything</span>
                    <Kbd>{mod}K</Kbd>
                  </button>
                </div>
                {tables.length > 0 && (
                  <>
                    <div className="ws-section-label">Tables in {ws.schema}</div>
                    <div className="ws-tables">
                      {tables.map((t) => (
                        <button key={t.name} className="ws-table" onClick={() => openTable(ws.sessionId, ws.schema, t)}>
                          <Table2 size={13} /> {t.name}
                        </button>
                      ))}
                    </div>
                  </>
                )}
              </div>
            </div>
          )}
          {ws.tabs.map((t) => {
            const active = visible && t.id === ws.activeTabId;
            return (
              <div key={t.id} className="tab-pane" style={{ display: t.id === ws.activeTabId ? 'flex' : 'none' }}>
                <ErrorBoundary label="this tab">
                  {t.kind === 'table' && <TableView ws={ws} tab={t} active={active} />}
                  {t.kind === 'query' && <QueryView ws={ws} tab={t} active={active} />}
                  {t.kind === 'diagram' && <ErDiagram ws={ws} tab={t} active={active} />}
                  {t.kind === 'object' && (
                    <DdlPanel ws={ws} schema={t.schema} name={t.name} kind={t.objectKind} signature={t.signature} />
                  )}
                </ErrorBoundary>
              </div>
            );
          })}
        </div>
      </main>
      {inspectorVisible && visible && (
        <>
          <Splitter direction="vertical" onDrag={(d) => setInspectorW((w) => Math.max(240, Math.min(640, w - d)))} />
          <ErrorBoundary label="the inspector" compact>
            <Inspector width={inspectorW} />
          </ErrorBoundary>
        </>
      )}
    </div>
  );
}
