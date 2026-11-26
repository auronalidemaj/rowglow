import { create } from 'zustand';
import { call, errorMessage, RpcError } from '../lib/rpc';
import { uid } from '../lib/format';
import type { Filter } from '../lib/sqlgen';
import {
  DEFAULT_SETTINGS,
  isFileEngine,
  isSqlEngine,
  type AppInfo,
  type CompletionTable,
  type ConnectionConfig,
  type DbObject,
  type Discovered,
  type LicenseStatus,
  type ObjectKind,
  type ServerInfo,
  type Settings,
  type Snippet,
  type UpdateInfo,
} from '../lib/types';

export interface TableTab {
  id: string;
  kind: 'table';
  schema: string;
  name: string;
  objectKind: ObjectKind;
  view: 'data' | 'structure' | 'ddl';
  preview: boolean;
  dirty?: boolean;
  initialFilters?: Filter[];
}

export interface QueryTab {
  id: string;
  kind: 'query';
  title: string;
  sql: string;
  snippetId?: string;
  filePath?: string;
  dirty?: boolean;
  autoRun?: boolean;
  /** Open the AI assistant when the tab shows. */
  askAi?: boolean;
}

export interface ObjectTab {
  id: string;
  kind: 'object';
  schema: string;
  name: string;
  objectKind: ObjectKind;
  signature?: string;
}

export interface DiagramTab {
  id: string;
  kind: 'diagram';
  schema: string;
  name: string;
}

export type Tab = TableTab | QueryTab | ObjectTab | DiagramTab;

export interface Workspace {
  /** Stable id for React keys; sessionId changes when switching databases. */
  key: string;
  sessionId: string;
  connection: ConnectionConfig;
  info: ServerInfo;
  schema: string;
  schemas: string[];
  databases: string[];
  objects: DbObject[];
  objectsLoading: boolean;
  objectsError?: string;
  tabs: Tab[];
  activeTabId: string | null;
  completions: CompletionTable[];
  sidebarMode: 'items' | 'queries' | 'history';
  /** Bumped to make open table tabs reload (e.g. after DDL in the editor). */
  refreshKey: number;
}

export interface Toast {
  id: string;
  kind: 'info' | 'success' | 'error';
  title: string;
  body?: string;
}

export type Modal =
  | {
      kind: 'connection';
      connection?: ConnectionConfig;
      preset?: Partial<ConnectionConfig>;
      presetPassword?: string;
    }
  | {
      kind: 'settings';
      section?: 'general' | 'editor' | 'ai' | 'license' | 'privacy' | 'advanced' | 'about';
    }
  | {
      kind: 'password';
      connection: ConnectionConfig;
      database?: string;
      error?: string;
    }
  | { kind: 'shortcuts' }
  | { kind: 'import'; sessionId: string; table?: string }
  | { kind: 'upgrade'; reason: string }
  | {
      kind: 'saveSnippet';
      sessionId: string;
      tabId: string;
      sql: string;
      name?: string;
      snippetId?: string;
    };

export interface ConfirmOptions {
  title: string;
  message?: string;
  code?: string;
  confirmLabel?: string;
  danger?: boolean;
  requireText?: string;
}

interface ConfirmState extends ConfirmOptions {
  resolve: (ok: boolean) => void;
}

interface State {
  ready: boolean;
  appInfo: AppInfo | null;
  settings: Settings;
  license: LicenseStatus | null;
  update: UpdateInfo | null;
  connections: ConnectionConfig[];
  workspaces: Workspace[];
  active: string | null;
  modal: Modal | null;
  confirmState: ConfirmState | null;
  paletteOpen: boolean;
  toasts: Toast[];
  sidebarVisible: boolean;
  inspectorVisible: boolean;
  radar: {
    scanning: boolean;
    results: Discovered[];
    scannedAt?: number;
    error?: string;
  };
  connecting: string | null;
  snippets: Snippet[];
  coreError: string | null;
}

interface Actions {
  init(): Promise<void>;
  setSettings(patch: Partial<Settings>): Promise<void>;
  refreshConnections(): Promise<void>;
  saveConnection(
    cfg: ConnectionConfig,
    password?: string,
    clearPassword?: boolean,
    ssh?: {
      sshPassword?: string;
      clearSshPassword?: boolean;
      sentinelPassword?: string;
      clearSentinelPassword?: boolean;
    },
  ): Promise<ConnectionConfig>;
  /** If `e` is an unknown / changed SSH host key, asks the user and trusts it. Returns true to retry. */
  trustSsh(e: unknown, conn: Pick<ConnectionConfig, 'sshHost' | 'sshPort'>): Promise<boolean>;
  deleteConnection(c: ConnectionConfig): Promise<void>;
  connect(connectionId: string, opts?: { database?: string; password?: string; savePassword?: boolean }): Promise<boolean>;
  disconnect(sessionId: string): Promise<void>;
  setActive(sessionId: string | null): void;
  switchDatabase(sessionId: string, database: string): Promise<void>;
  setSchema(sessionId: string, schema: string): Promise<void>;
  loadObjects(sessionId: string): Promise<void>;
  loadCompletions(sessionId: string): Promise<void>;
  bumpRefresh(sessionId: string): void;
  openTable(
    sessionId: string,
    schema: string,
    obj: { name: string; kind: ObjectKind },
    opts?: { preview?: boolean; view?: TableTab['view']; filters?: Filter[] },
  ): void;
  openObject(sessionId: string, schema: string, obj: DbObject): void;
  openQuery(
    sessionId: string,
    opts?: {
      sql?: string;
      title?: string;
      autoRun?: boolean;
      snippetId?: string;
      filePath?: string;
      askAi?: boolean;
    },
  ): void;
  /** Opens (or focuses) the ER diagram of the workspace's current schema. */
  openDiagram(sessionId: string): void;
  closeTab(sessionId: string, tabId: string, force?: boolean): Promise<void>;
  setActiveTab(sessionId: string, tabId: string): void;
  updateTab(sessionId: string, tabId: string, patch: Partial<Tab>): void;
  updateWorkspace(sessionId: string, patch: Partial<Workspace>): void;
  toast(kind: Toast['kind'], title: string, body?: string): void;
  dismissToast(id: string): void;
  confirm(opts: ConfirmOptions): Promise<boolean>;
  resolveConfirm(ok: boolean): void;
  openModal(m: Modal | null): void;
  setPalette(open: boolean): void;
  toggleSidebar(): void;
  toggleInspector(): void;
  scanRadar(): Promise<void>;
  refreshLicense(): Promise<void>;
  loadSnippets(): Promise<void>;
  checkUpdates(manual?: boolean): Promise<void>;
  handleLimitError(e: unknown): boolean;
}

export type Store = State & Actions;

function applyTheme(theme: Settings['theme']) {
  const dark = theme === 'dark' || (theme === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches);
  document.documentElement.dataset.theme = dark ? 'dark' : 'light';
  try {
    localStorage.setItem('rowglow.theme', theme);
  } catch {
    /* ignore */
  }
}

function applyDensity(d: Settings['density']) {
  document.documentElement.dataset.density = d;
}

const isAuthError = (e: unknown) => {
  const m = errorMessage(e).toLowerCase();
  return (
    m.includes('password') ||
    m.includes('authentication') ||
    m.includes('access denied') ||
    m.includes('no password supplied') ||
    m.includes('28p01') ||
    m.includes('1045')
  );
};

export const useStore = create<Store>((set, get) => {
  const ws = (sessionId: string) => get().workspaces.find((w) => w.sessionId === sessionId);
  const patchWs = (sessionId: string, fn: (w: Workspace) => Partial<Workspace>) =>
    set((s) => ({
      workspaces: s.workspaces.map((w) => (w.sessionId === sessionId ? { ...w, ...fn(w) } : w)),
    }));

  return {
    ready: false,
    appInfo: null,
    settings: DEFAULT_SETTINGS,
    license: null,
    update: null,
    connections: [],
    workspaces: [],
    active: null,
    modal: null,
    confirmState: null,
    paletteOpen: false,
    toasts: [],
    sidebarVisible: true,
    inspectorVisible: false,
    radar: { scanning: false, results: [] },
    connecting: null,
    snippets: [],
    coreError: null,

    async init() {
      try {
        const [appInfo, rawSettings, license, connections, snippets] = await Promise.all([
          call<AppInfo>('app.info'),
          call<Partial<Settings>>('settings.get'),
          call<LicenseStatus>('license.status'),
          call<ConnectionConfig[]>('connections.list'),
          call<Snippet[]>('snippets.list'),
        ]);
        const settings = { ...DEFAULT_SETTINGS, ...rawSettings };
        applyTheme(settings.theme);
        applyDensity(settings.density);
        set({
          appInfo,
          settings,
          license,
          connections,
          snippets,
          ready: true,
          coreError: null,
        });
        window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
          if (get().settings.theme === 'system') applyTheme('system');
        });
        // Local Radar only runs after onboarding, and only if the user left it on.
        if (settings.onboarded && settings.radarOnLaunch) void get().scanRadar();
        setTimeout(() => get().checkUpdates(false), 4000);
        if (appInfo.recoveredDb) {
          void get().confirm({
            title: 'Rowglow’s database was damaged',
            message:
              'It couldn’t be opened, so Rowglow moved it aside and started a new one. Your earlier saved connections, history and saved queries are in this file; passwords in the system keychain are unaffected.',
            code: appInfo.recoveredDb,
            confirmLabel: 'OK',
          });
        }
      } catch (e) {
        set({ coreError: errorMessage(e), ready: false });
      }
    },

    async setSettings(patch) {
      const next = { ...get().settings, ...patch };
      set({ settings: next });
      if (patch.theme) applyTheme(patch.theme);
      if (patch.density) applyDensity(patch.density);
      await call('settings.set', { patch });
    },

    async refreshConnections() {
      set({ connections: await call<ConnectionConfig[]>('connections.list') });
    },

    async saveConnection(cfg, password, clearPassword, ssh) {
      const saved = await call<ConnectionConfig>('connections.save', {
        connection: cfg,
        password: password ?? null,
        clearPassword: !!clearPassword,
        sshPassword: ssh?.sshPassword ?? null,
        clearSshPassword: !!ssh?.clearSshPassword,
        sentinelPassword: ssh?.sentinelPassword ?? null,
        clearSentinelPassword: !!ssh?.clearSentinelPassword,
      });
      await get().refreshConnections();
      if (password || ssh?.sshPassword || ssh?.sentinelPassword) {
        // Tell the user once if the keychain refused and the password went into app.db.
        const before = get().appInfo?.secretsInFile;
        const info = await call<AppInfo>('app.info').catch(() => null);
        if (info) {
          set({ appInfo: info });
          if (info.secretsInFile && !before) {
            get().toast(
              'error',
              'Password saved without the system keychain',
              'No keychain was available, so it’s stored unencrypted in Rowglow’s data folder (readable only by your user account).',
            );
          }
        }
      }
      // Keep any open workspace in sync with edited settings (name, color, env…)
      set((s) => ({
        workspaces: s.workspaces.map((w) => (w.connection.id === saved.id ? { ...w, connection: saved } : w)),
      }));
      return saved;
    },

    async trustSsh(e, conn) {
      if (!(e instanceof RpcError) || !e.detail) return false;
      const host = conn.sshHost.trim();
      const port = conn.sshPort || 22;
      if (e.code === 'ssh_unknown_host') {
        const ok = await get().confirm({
          title: `Trust the SSH server ${host}?`,
          message:
            'This is the first connection to this server. Check that its host key fingerprint matches the one your server or hosting provider shows (ssh-keygen -lf on the server). Rowglow remembers it and refuses to connect if it ever changes.',
          code: e.detail,
          confirmLabel: 'Trust and connect',
        });
        if (!ok) return false;
        await call('ssh.trust', { host, port, fingerprint: e.detail });
        return true;
      }
      if (e.code === 'ssh_host_key_changed') {
        const ok = await get().confirm({
          title: `The SSH server ${host} has a different key`,
          message:
            'Its host key changed since you last connected. That happens when a server is rebuilt, but it can also mean someone is intercepting the connection. Only continue if you know the server was reinstalled. New fingerprint:',
          code: e.detail,
          confirmLabel: 'Trust the new key',
          danger: true,
          requireText: 'trust',
        });
        if (!ok) return false;
        await call('ssh.forget', { host, port });
        await call('ssh.trust', { host, port, fingerprint: e.detail });
        return true;
      }
      return false;
    },

    async deleteConnection(c) {
      const ok = await get().confirm({
        title: `Delete “${c.name}”?`,
        message: 'The saved connection, its password, query history and saved queries for it will be removed. Your database is not touched.',
        confirmLabel: 'Delete',
        danger: true,
      });
      if (!ok) return;
      for (const w of get().workspaces.filter((w) => w.connection.id === c.id)) await get().disconnect(w.sessionId);
      await call('connections.delete', { id: c.id });
      await get().refreshConnections();
      get().toast('success', `Deleted ${c.name}`);
    },

    async connect(connectionId, opts = {}) {
      const existing = get().workspaces.find((w) => w.connection.id === connectionId && (!opts.database || w.info.database === opts.database));
      if (existing && !opts.password) {
        set({ active: existing.sessionId });
        return true;
      }
      const conn = get().connections.find((c) => c.id === connectionId);
      if (!conn) return false;
      // A double-click must not open two sessions.
      if (get().connecting === connectionId && !opts.password) return false;
      set({ connecting: connectionId });
      try {
        const open = () =>
          call<ServerInfo>('session.open', {
            connectionId,
            database: opts.database ?? null,
            password: opts.password ?? null,
            savePassword: !!opts.savePassword,
          });
        let info: ServerInfo;
        try {
          info = await open();
        } catch (e) {
          if (!(await get().trustSsh(e, conn))) throw e;
          info = await open();
        }
        const schema = info.defaultSchema || (conn.engine === 'postgres' ? 'public' : conn.engine === 'mssql' ? 'dbo' : info.database || 'main');
        const w: Workspace = {
          key: uid(),
          sessionId: info.sessionId,
          connection: conn,
          info,
          schema,
          schemas: [schema],
          databases: [info.database],
          objects: [],
          objectsLoading: true,
          tabs: [],
          activeTabId: null,
          completions: [],
          sidebarMode: 'items',
          refreshKey: 0,
        };
        set((s) => ({
          workspaces: [...s.workspaces, w],
          active: info.sessionId,
          connecting: null,
          modal: null,
        }));
        void get().refreshConnections();
        const sid = info.sessionId;
        if (!isSqlEngine(conn.engine)) {
          patchWs(sid, () => ({ objectsLoading: false }));
          if (conn.engine === 'redis') {
            call<string[]>('meta.databases', { sessionId: sid })
              .then((databases) => patchWs(sid, () => ({ databases })))
              .catch(() => {});
          }
          return true;
        }
        const schemasReady = call<string[]>('meta.schemas', { sessionId: sid })
          .then(async (schemas) => {
            patchWs(sid, () => ({ schemas }));
            // MySQL without a default database: start in the first user database.
            if (!schemas.includes(schema) && schemas.length) {
              const system = ['information_schema', 'mysql', 'performance_schema', 'sys'];
              const first = schemas.find((x) => !system.includes(x)) ?? schemas[0];
              await get().setSchema(sid, first);
              return false;
            }
            return true;
          })
          .catch(() => true);
        if (conn.engine === 'postgres' || conn.engine === 'mssql') {
          call<string[]>('meta.databases', { sessionId: sid })
            .then((databases) => patchWs(sid, () => ({ databases })))
            .catch(() => {});
        }
        if (conn.engine === 'mysql' && !info.database) {
          await schemasReady;
        } else {
          await get().loadObjects(sid);
          void get().loadCompletions(sid);
        }
        return true;
      } catch (e) {
        set({ connecting: null });
        const signInWithoutPassword = conn.engine === 'mssql' && ['integrated', 'entra-cli'].includes(conn.options?.auth ?? '');
        const entraError = e instanceof RpcError && (e.code === 'entra_failed' || e.code === 'az_missing');
        if (
          isAuthError(e) &&
          !isFileEngine(conn.engine) &&
          !signInWithoutPassword &&
          !entraError &&
          !(e instanceof RpcError && e.code?.startsWith('ssh_'))
        ) {
          set({
            modal: {
              kind: 'password',
              connection: conn,
              database: opts.database,
              error: opts.password ? errorMessage(e) : undefined,
            },
          });
          return false;
        }
        get().toast('error', `Couldn’t connect to ${conn.name}`, errorMessage(e));
        return false;
      }
    },

    async disconnect(sessionId) {
      const w = ws(sessionId);
      if (!w) return;
      if (w.tabs.some((t) => (t.kind === 'table' || t.kind === 'query') && t.dirty && t.kind === 'table')) {
        const ok = await get().confirm({
          title: 'Discard staged changes?',
          message: `${w.connection.name} has uncommitted edits. Disconnecting discards them.`,
          confirmLabel: 'Disconnect',
          danger: true,
        });
        if (!ok) return;
      }
      await call('session.close', { sessionId }).catch(() => {});
      set((s) => {
        const workspaces = s.workspaces.filter((x) => x.sessionId !== sessionId);
        const idx = s.workspaces.findIndex((x) => x.sessionId === sessionId);
        const active = s.active === sessionId ? (workspaces[Math.min(idx, workspaces.length - 1)]?.sessionId ?? null) : s.active;
        return { workspaces, active };
      });
    },

    setActive(sessionId) {
      set({ active: sessionId });
    },

    async switchDatabase(sessionId, database) {
      const w = ws(sessionId);
      if (!w || w.info.database === database) return;
      if (w.connection.engine === 'mysql') {
        await get().setSchema(sessionId, database);
        return;
      }
      if (w.tabs.some((t) => t.kind === 'table' && t.dirty)) {
        const ok = await get().confirm({
          title: 'Switch database?',
          message: 'Open tables have staged changes that will be discarded.',
          confirmLabel: 'Switch',
          danger: true,
        });
        if (!ok) return;
      }
      try {
        const info = await call<ServerInfo>('session.open', {
          connectionId: w.connection.id,
          database,
        });
        await call('session.close', { sessionId }).catch(() => {});
        const schema = info.defaultSchema || (w.connection.engine === 'mssql' ? 'dbo' : 'public');
        set((s) => ({
          workspaces: s.workspaces.map((x) =>
            x.sessionId === sessionId
              ? {
                  ...x,
                  sessionId: info.sessionId,
                  info,
                  schema,
                  objects: [],
                  objectsLoading: true,
                  tabs: x.tabs.filter((t) => t.kind === 'query'),
                  activeTabId: x.tabs.find((t) => t.kind === 'query')?.id ?? null,
                  completions: [],
                }
              : x,
          ),
          active: s.active === sessionId ? info.sessionId : s.active,
        }));
        const sid = info.sessionId;
        if (!isSqlEngine(w.connection.engine)) {
          patchWs(sid, (x) => ({
            objectsLoading: false,
            databases: w.databases.length ? w.databases : x.databases,
          }));
          return;
        }
        call<string[]>('meta.schemas', { sessionId: sid })
          .then((schemas) => patchWs(sid, () => ({ schemas })))
          .catch(() => {});
        await get().loadObjects(sid);
        void get().loadCompletions(sid);
      } catch (e) {
        get().toast('error', `Couldn’t open ${database}`, errorMessage(e));
      }
    },

    async setSchema(sessionId, schema) {
      const w = ws(sessionId);
      if (!w) return;
      patchWs(sessionId, () => ({ schema, objects: [], objectsLoading: true }));
      call('session.useSchema', { sessionId, schema }).catch(() => {});
      await get().loadObjects(sessionId);
      if (w.connection.engine === 'mysql') {
        patchWs(sessionId, (x) => ({ info: { ...x.info, database: schema } }));
        void get().loadCompletions(sessionId);
      }
    },

    async loadObjects(sessionId) {
      const w = ws(sessionId);
      if (!w) return;
      patchWs(sessionId, () => ({
        objectsLoading: true,
        objectsError: undefined,
      }));
      try {
        const objects = await call<DbObject[]>('meta.objects', {
          sessionId,
          schema: w.schema,
        });
        patchWs(sessionId, () => ({ objects, objectsLoading: false }));
      } catch (e) {
        patchWs(sessionId, () => ({
          objectsLoading: false,
          objectsError: errorMessage(e),
        }));
      }
    },

    async loadCompletions(sessionId) {
      const w = ws(sessionId);
      if (!w) return;
      try {
        const completions = await call<CompletionTable[]>('meta.completions', {
          sessionId,
          schema: w.schema,
        });
        patchWs(sessionId, () => ({ completions }));
      } catch {
        /* autocomplete is best-effort */
      }
    },

    bumpRefresh(sessionId) {
      patchWs(sessionId, (w) => ({ refreshKey: w.refreshKey + 1 }));
    },

    openTable(sessionId, schema, obj, opts = {}) {
      const w = ws(sessionId);
      if (!w) return;
      const preview = opts.preview ?? false;
      const existing = w.tabs.find((t) => t.kind === 'table' && t.schema === schema && t.name === obj.name && !opts.filters) as TableTab | undefined;
      if (existing) {
        patchWs(sessionId, (x) => ({
          activeTabId: existing.id,
          tabs: x.tabs.map((t) =>
            t.id === existing.id
              ? {
                  ...t,
                  preview: preview && existing.preview,
                  ...(opts.view ? { view: opts.view } : {}),
                }
              : t,
          ),
        }));
        return;
      }
      const tab: TableTab = {
        id: uid(),
        kind: 'table',
        schema,
        name: obj.name,
        objectKind: obj.kind,
        view: opts.view ?? 'data',
        preview,
        initialFilters: opts.filters,
      };
      patchWs(sessionId, (x) => {
        // Replace the current preview tab (if clean) instead of piling up tabs.
        const replaceIdx = preview ? x.tabs.findIndex((t) => t.kind === 'table' && t.preview && !t.dirty) : -1;
        const tabs = [...x.tabs];
        if (replaceIdx >= 0) tabs[replaceIdx] = tab;
        else {
          const activeIdx = tabs.findIndex((t) => t.id === x.activeTabId);
          tabs.splice(activeIdx >= 0 ? activeIdx + 1 : tabs.length, 0, tab);
        }
        return { tabs, activeTabId: tab.id };
      });
    },

    openObject(sessionId, schema, obj) {
      const w = ws(sessionId);
      if (!w) return;
      const existing = w.tabs.find((t) => t.kind === 'object' && t.schema === schema && t.name === obj.name && t.signature === obj.signature);
      if (existing) {
        patchWs(sessionId, () => ({ activeTabId: existing.id }));
        return;
      }
      const tab: ObjectTab = {
        id: uid(),
        kind: 'object',
        schema,
        name: obj.name,
        objectKind: obj.kind,
        signature: obj.signature,
      };
      patchWs(sessionId, (x) => ({
        tabs: [...x.tabs, tab],
        activeTabId: tab.id,
      }));
    },

    openDiagram(sessionId) {
      const w = ws(sessionId);
      if (!w) return;
      const existing = w.tabs.find((t) => t.kind === 'diagram' && t.schema === w.schema);
      if (existing) {
        patchWs(sessionId, () => ({ activeTabId: existing.id }));
        return;
      }
      const tab: DiagramTab = {
        id: uid(),
        kind: 'diagram',
        schema: w.schema,
        name: `Diagram · ${w.schema}`,
      };
      patchWs(sessionId, (x) => ({
        tabs: [...x.tabs, tab],
        activeTabId: tab.id,
      }));
    },

    openQuery(sessionId, opts = {}) {
      const w = ws(sessionId);
      if (!w) return;
      if (opts.snippetId) {
        const ex = w.tabs.find((t) => t.kind === 'query' && t.snippetId === opts.snippetId);
        if (ex) {
          patchWs(sessionId, () => ({ activeTabId: ex.id }));
          return;
        }
      }
      if (opts.askAi) {
        // Reuse the active query tab if there is one.
        const cur = w.tabs.find((t) => t.id === w.activeTabId && t.kind === 'query');
        if (cur) {
          patchWs(sessionId, (x) => ({
            tabs: x.tabs.map((t) => (t.id === cur.id ? { ...t, askAi: true } : t)),
          }));
          return;
        }
      }
      const n = w.tabs.filter((t) => t.kind === 'query').length + 1;
      const tab: QueryTab = {
        id: uid(),
        kind: 'query',
        title: opts.title ?? `Query ${n}`,
        sql: opts.sql ?? '',
        autoRun: opts.autoRun,
        askAi: opts.askAi,
        snippetId: opts.snippetId,
        filePath: opts.filePath,
      };
      patchWs(sessionId, (x) => ({
        tabs: [...x.tabs, tab],
        activeTabId: tab.id,
      }));
    },

    async closeTab(sessionId, tabId, force) {
      const w = ws(sessionId);
      const tab = w?.tabs.find((t) => t.id === tabId);
      if (!w || !tab) return;
      if (!force && tab.kind === 'table' && tab.dirty) {
        const ok = await get().confirm({
          title: `Discard changes to ${tab.name}?`,
          message: 'This tab has staged edits that haven’t been committed.',
          confirmLabel: 'Discard',
          danger: true,
        });
        if (!ok) return;
      }
      patchWs(sessionId, (x) => {
        const idx = x.tabs.findIndex((t) => t.id === tabId);
        const tabs = x.tabs.filter((t) => t.id !== tabId);
        const activeTabId = x.activeTabId === tabId ? (tabs[Math.min(idx, tabs.length - 1)]?.id ?? null) : x.activeTabId;
        return { tabs, activeTabId };
      });
    },

    setActiveTab(sessionId, tabId) {
      patchWs(sessionId, () => ({ activeTabId: tabId }));
    },

    updateTab(sessionId, tabId, patch) {
      patchWs(sessionId, (x) => ({
        tabs: x.tabs.map((t) => (t.id === tabId ? ({ ...t, ...patch } as Tab) : t)),
      }));
    },

    updateWorkspace(sessionId, patch) {
      patchWs(sessionId, () => patch);
    },

    toast(kind, title, body) {
      const id = uid();
      set((s) => ({
        toasts: [...s.toasts.slice(-4), { id, kind, title, body }],
      }));
      setTimeout(() => get().dismissToast(id), kind === 'error' ? 8000 : 3500);
    },

    dismissToast(id) {
      set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }));
    },

    confirm(opts) {
      return new Promise<boolean>((resolve) => {
        set({ confirmState: { ...opts, resolve } });
      });
    },

    resolveConfirm(ok) {
      const c = get().confirmState;
      set({ confirmState: null });
      c?.resolve(ok);
    },

    openModal(m) {
      set({ modal: m });
    },

    setPalette(open) {
      set({ paletteOpen: open });
    },

    toggleSidebar() {
      set((s) => ({ sidebarVisible: !s.sidebarVisible }));
    },

    toggleInspector() {
      set((s) => ({ inspectorVisible: !s.inspectorVisible }));
    },

    async scanRadar() {
      if (get().radar.scanning) return;
      set((s) => ({ radar: { ...s.radar, scanning: true, error: undefined } }));
      try {
        const results = await call<Discovered[]>('radar.scan');
        set({ radar: { scanning: false, results, scannedAt: Date.now() } });
      } catch (e) {
        set((s) => ({
          radar: { ...s.radar, scanning: false, error: errorMessage(e) },
        }));
      }
    },

    async refreshLicense() {
      set({ license: await call<LicenseStatus>('license.status') });
    },

    async loadSnippets() {
      set({ snippets: await call<Snippet[]>('snippets.list') });
    },

    async checkUpdates(manual) {
      try {
        const update = await call<UpdateInfo>('updates.check');
        set({ update });
        // The update check also carries the free launch period; it may have moved.
        void get().refreshLicense().catch(() => {});
        if (manual) {
          if (update.updateAvailable) get().toast('info', `Rowglow ${update.version} is available`, 'Open Settings → About to download it.');
          else get().toast('success', 'You’re up to date', `Rowglow ${get().appInfo?.version ?? ''} is the latest version.`);
        }
      } catch (e) {
        if (manual) get().toast('error', 'Couldn’t check for updates', errorMessage(e));
      }
    },

    handleLimitError(e) {
      if (e instanceof RpcError && e.kind === 'license') {
        set({ modal: { kind: 'upgrade', reason: e.message } });
        return true;
      }
      return false;
    },
  };
});

export const activeWorkspace = (s: Store) => s.workspaces.find((w) => w.sessionId === s.active) ?? null;

// State modules hold live sessions; a hot swap would orphan them, so reload the page instead.
if (import.meta.hot) import.meta.hot.accept(() => window.location.reload());
