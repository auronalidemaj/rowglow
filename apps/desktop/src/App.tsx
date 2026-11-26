import { AlertTriangle, RefreshCw } from 'lucide-react';
import { useCallback, useEffect } from 'react';
import { CommandPalette } from './components/CommandPalette';
import { ConnectionForm } from './components/ConnectionForm';
import { Rail, StatusBar, TitleBar } from './components/Chrome';
import { ConfirmDialog, Onboarding, PasswordPrompt, SaveSnippetModal, ShortcutsModal, Toasts, UpgradeModal } from './components/Dialogs';
import { Home } from './components/Home';
import { ImportModal } from './components/ImportModal';
import { ErrorBoundary } from './components/ErrorBoundary';
import { SettingsModal } from './components/SettingsModal';
import { Button, LogoMark, Spinner } from './components/ui';
import { WorkspaceView } from './components/Workspace';
import { matches, overlayOpen } from './lib/hotkeys';
import { onMenu, openExternal, pickOpenFile } from './lib/platform';
import { call, errorMessage, isTauri } from './lib/rpc';
import { activeWorkspace, useStore } from './state/store';

export default function App() {
  const ready = useStore((s) => s.ready);
  const coreError = useStore((s) => s.coreError);
  const workspaces = useStore((s) => s.workspaces);
  const active = useStore((s) => s.active);
  const modal = useStore((s) => s.modal);
  const importWs = useStore((s) => (s.modal?.kind === 'import' ? s.workspaces.find((w) => w.sessionId === (s.modal as { sessionId: string }).sessionId) : undefined));
  const onboarded = useStore((s) => s.settings.onboarded);

  useEffect(() => {
    void useStore.getState().init();
  }, []);

  const openSqlFile = useCallback(async () => {
    const st = useStore.getState();
    const ws = activeWorkspace(st);
    if (!ws) {
      st.toast('info', 'Connect to a database first', 'SQL files open in a query tab of the active connection.');
      return;
    }
    const path = await pickOpenFile('Open SQL file', [{ name: 'SQL', extensions: ['sql', 'txt'] }]);
    if (!path) return;
    try {
      const sql = await call<string>('fs.readText', { path });
      st.openQuery(ws.sessionId, { sql, title: path.split('/').pop() ?? 'SQL', filePath: path });
    } catch (e) {
      st.toast('error', 'Couldn’t open file', errorMessage(e));
    }
  }, []);

  // Global shortcuts
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const st = useStore.getState();
      if (matches(e, 'mod+k') || matches(e, 'mod+p')) {
        e.preventDefault();
        st.setPalette(!st.paletteOpen);
        return;
      }
      if (overlayOpen()) return;
      const ws = activeWorkspace(st);
      const go = (fn: () => void) => {
        e.preventDefault();
        fn();
      };
      if (matches(e, 'mod+,')) return go(() => st.openModal({ kind: 'settings' }));
      if (matches(e, 'mod+n')) return go(() => st.openModal({ kind: 'connection' }));
      if (matches(e, 'mod+o')) return go(() => void openSqlFile());
      if (matches(e, 'mod+0')) return go(() => st.setActive(null));
      if (matches(e, 'mod+\\')) return go(st.toggleSidebar);
      if (matches(e, 'mod+shift+i')) return go(st.toggleInspector);
      if (matches(e, 'mod+/') && !(e.target as HTMLElement)?.closest?.('.cm-editor')) return go(() => st.openModal({ kind: 'shortcuts' }));
      for (let i = 1; i <= 9; i++) {
        if (matches(e, `mod+${i}`)) {
          const w = st.workspaces[i - 1];
          if (w) return go(() => st.setActive(w.sessionId));
        }
      }
      if (!ws) return;
      if (matches(e, 'mod+t')) return go(() => st.openQuery(ws.sessionId));
      if (matches(e, 'mod+w')) return go(() => ws.activeTabId && void st.closeTab(ws.sessionId, ws.activeTabId));
      const cycle = (dir: number) => {
        if (!ws.tabs.length) return;
        const i = ws.tabs.findIndex((t) => t.id === ws.activeTabId);
        const next = ws.tabs[(i + dir + ws.tabs.length) % ws.tabs.length];
        st.setActiveTab(ws.sessionId, next.id);
      };
      if (matches(e, 'mod+shift+]') || (e.ctrlKey && e.key === 'Tab' && !e.shiftKey)) return go(() => cycle(1));
      if (matches(e, 'mod+shift+[') || (e.ctrlKey && e.key === 'Tab' && e.shiftKey)) return go(() => cycle(-1));
      if (matches(e, 'mod+alt+arrowright')) return go(() => cycle(1));
      if (matches(e, 'mod+alt+arrowleft')) return go(() => cycle(-1));
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [openSqlFile]);

  // Native menu (Tauri)
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void onMenu((id) => {
      const st = useStore.getState();
      const ws = activeWorkspace(st);
      switch (id) {
        case 'settings':
          return st.openModal({ kind: 'settings' });
        case 'license':
          return st.openModal({ kind: 'settings', section: 'license' });
        case 'updates':
          return void st.checkUpdates(true);
        case 'new-connection':
          return st.openModal({ kind: 'connection' });
        case 'new-query':
          return ws ? st.openQuery(ws.sessionId) : st.toast('info', 'Connect to a database first');
        case 'open-sql':
          return void openSqlFile();
        case 'radar':
          st.setActive(null);
          return void st.scanRadar();
        case 'palette':
          return st.setPalette(true);
        case 'toggle-sidebar':
          return st.toggleSidebar();
        case 'toggle-inspector':
          return st.toggleInspector();
        case 'shortcuts':
          return st.openModal({ kind: 'shortcuts' });
        case 'website':
          if (st.appInfo?.apiBase) return void openExternal(st.appInfo.apiBase);
          return st.toast('info', 'No website configured', 'Set it in Settings → Advanced.');
      }
    }).then((u) => (unlisten = u));
    return () => unlisten?.();
  }, [openSqlFile]);

  // Staged (uncommitted) table edits: tell the native shell so quitting or
  // closing the window asks first, and confirm when it does.
  useEffect(() => {
    if (!isTauri) return;
    let last: boolean | null = null;
    const dirtyCount = (s: ReturnType<typeof useStore.getState>) =>
      s.workspaces.reduce((n, w) => n + w.tabs.filter((t) => t.kind === 'table' && t.dirty).length, 0);
    const sync = async (s: ReturnType<typeof useStore.getState>) => {
      const dirty = dirtyCount(s) > 0;
      if (dirty === last) return;
      last = dirty;
      const { invoke } = await import('@tauri-apps/api/core');
      await invoke('set_unsaved', { dirty }).catch(() => {});
    };
    void sync(useStore.getState());
    const unsub = useStore.subscribe((s) => void sync(s));
    let unlisten: (() => void) | undefined;
    void import('@tauri-apps/api/event').then(({ listen }) =>
      listen('confirm-quit', async () => {
        const st = useStore.getState();
        const n = dirtyCount(st);
        const ok = await st.confirm({
          title: `Quit and discard unsaved edits?`,
          message: `${n === 1 ? 'A table has' : `${n} tables have`} staged changes that haven’t been committed. They’ll be lost if you quit now.`,
          confirmLabel: 'Quit without saving',
          danger: true,
        });
        if (ok) {
          const { invoke } = await import('@tauri-apps/api/core');
          await invoke('quit_app');
        }
      }).then((u) => (unlisten = u)),
    );
    return () => {
      unsub();
      unlisten?.();
    };
  }, []);

  // Prevent the webview's default context menu and file drops navigating away.
  useEffect(() => {
    const prevent = (e: Event) => {
      const t = e.target as HTMLElement;
      if (t.closest('input, textarea, .cm-editor')) return;
      e.preventDefault();
    };
    const noDrop = (e: DragEvent) => e.preventDefault();
    window.addEventListener('contextmenu', prevent);
    window.addEventListener('dragover', noDrop);
    window.addEventListener('drop', noDrop);
    return () => {
      window.removeEventListener('contextmenu', prevent);
      window.removeEventListener('dragover', noDrop);
      window.removeEventListener('drop', noDrop);
    };
  }, []);

  if (coreError) {
    return (
      <div className="boot-screen">
        <LogoMark size={40} />
        <div className="boot-error">
          <AlertTriangle size={16} /> {coreError}
        </div>
        <Button onClick={() => useStore.getState().init()}>
          <RefreshCw size={13} /> Retry
        </Button>
      </div>
    );
  }
  if (!ready) {
    return (
      <div className="boot-screen">
        <LogoMark size={40} />
        <Spinner size={18} />
      </div>
    );
  }

  return (
    <div className="app">
      <TitleBar />
      <div className="app-body">
        <Rail />
        <div className="stage">
          {active === null && (
            <ErrorBoundary label="the connections screen">
              <Home />
            </ErrorBoundary>
          )}
          {workspaces.map((w) => (
            <WorkspaceView key={w.key} ws={w} visible={w.sessionId === active} />
          ))}
        </div>
      </div>
      <StatusBar />
      <CommandPalette onOpenSqlFile={openSqlFile} />
      {modal?.kind === 'connection' && <ConnectionForm initial={modal.connection} preset={modal.preset} presetPassword={modal.presetPassword} />}
      {modal?.kind === 'settings' && <SettingsModal section={modal.section} />}
      {modal?.kind === 'password' && <PasswordPrompt connectionId={modal.connection.id} database={modal.database} error={modal.error} />}
      {modal?.kind === 'upgrade' && <UpgradeModal reason={modal.reason} />}
      {modal?.kind === 'shortcuts' && <ShortcutsModal />}
      {modal?.kind === 'saveSnippet' && <SaveSnippetModal sessionId={modal.sessionId} tabId={modal.tabId} sql={modal.sql} name={modal.name} />}
      {modal?.kind === 'import' && importWs && <ImportModal ws={importWs} table={modal.table} />}
      {!onboarded && !modal && <Onboarding />}
      <ConfirmDialog />
      <Toasts />
    </div>
  );
}
