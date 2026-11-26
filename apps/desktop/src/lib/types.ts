// Types mirrored from the Rust core (src-tauri/src/model.rs).

export type Engine = 'postgres' | 'mysql' | 'sqlite' | 'mssql' | 'duckdb' | 'redis' | 'mongodb';

/** Engines that speak SQL (tables, grid, editor, structure). */
export const isSqlEngine = (e: Engine) => e !== 'redis' && e !== 'mongodb';
/** Engines that open a local file instead of a server. */
export const isFileEngine = (e: Engine) => e === 'sqlite' || e === 'duckdb';
export type SslMode = 'disable' | 'prefer' | 'require';
export type EnvTag = 'local' | 'development' | 'staging' | 'production';

export interface ConnectionConfig {
  id: string;
  name: string;
  engine: Engine;
  host: string;
  port: number;
  username: string;
  database: string;
  sslMode: SslMode;
  filePath: string;
  color: string;
  env: EnvTag | string;
  readOnly: boolean;
  groupName: string;
  createdAt: number;
  updatedAt: number;
  lastUsedAt?: number | null;
  hasPassword: boolean;
  /** SSH tunnel: reach the database through an SSH server. */
  sshEnabled: boolean;
  sshHost: string;
  sshPort: number;
  sshUser: string;
  sshAuth: 'password' | 'key' | string;
  sshKeyPath: string;
  hasSshSecret?: boolean;
  /** Engine-specific settings. */
  options?: ConnOptions;
}

export type MssqlAuth = 'sql' | 'windows' | 'integrated' | 'entra-cli' | 'entra-sp';
export type RedisMode = 'standalone' | 'cluster' | 'sentinel';

export interface ConnOptions {
  /** SQL Server sign-in method. */
  auth?: MssqlAuth | '';
  /** Microsoft Entra tenant ID or domain. */
  tenant?: string;
  redisMode?: RedisMode | '';
  /** Cluster seed nodes or Sentinel nodes, host:port. */
  redisNodes?: string;
  sentinelMaster?: string;
  sentinelUser?: string;
  /** Sentinel: connect to a replica (read-only). */
  redisReplica?: boolean;
}

export interface Column {
  name: string;
  typeName: string;
}

export type Cell = string | null;

export interface QueryResult {
  columns: Column[];
  rows: Cell[][];
  hasRows: boolean;
  rowsAffected?: number | null;
  truncated: boolean;
  durationMs: number;
  notices: string[];
}

export interface AppErrorShape {
  kind: string;
  message: string;
  code?: string;
  detail?: string;
  hint?: string;
  position?: number;
}

export interface StatementOutcome {
  sql: string;
  result?: QueryResult;
  error?: AppErrorShape;
}

export interface ServerInfo {
  sessionId: string;
  connectionId: string;
  engine: Engine;
  flavor: string;
  version: string;
  database: string;
  defaultSchema: string;
  user: string;
  readOnly: boolean;
}

export type ObjectKind = 'table' | 'view' | 'matview' | 'foreign' | 'sequence' | 'function' | 'procedure' | 'type';

export interface DbObject {
  name: string;
  kind: ObjectKind;
  rowsEstimate?: number;
  comment?: string;
  signature?: string;
}

export interface ColumnDef {
  name: string;
  dataType: string;
  nullable: boolean;
  defaultValue: string | null;
  isPrimaryKey: boolean;
  comment: string | null;
  extra: string | null;
}

export interface IndexDef {
  name: string;
  columns: string[];
  unique: boolean;
  primary: boolean;
  definition: string | null;
}

export interface ForeignKeyDef {
  name: string;
  columns: string[];
  refSchema: string;
  refTable: string;
  refColumns: string[];
  onUpdate: string;
  onDelete: string;
}

export interface ConstraintDef {
  name: string;
  kind: string;
  definition: string;
}

export interface TriggerDef {
  name: string;
  definition: string;
}

export interface TableStructure {
  columns: ColumnDef[];
  primaryKey: string[];
  indexes: IndexDef[];
  foreignKeys: ForeignKeyDef[];
  constraints: ConstraintDef[];
  triggers: TriggerDef[];
  referencedBy: ForeignKeyDef[];
}

export interface CompletionTable {
  schema: string;
  name: string;
  kind: string;
  columns: [string, string][];
}

export interface ForeignKeyRelation {
  name: string;
  schema: string;
  table: string;
  columns: string[];
  refSchema: string;
  refTable: string;
  refColumns: string[];
}

export interface SchemaRelations {
  foreignKeys: ForeignKeyRelation[];
  primaryKeys: { table: string; columns: string[] }[];
}

export interface HistoryEntry {
  id: number;
  connectionId: string;
  database: string;
  sql: string;
  durationMs: number;
  rows: number | null;
  error: string | null;
  createdAt: number;
}

export interface Snippet {
  id: string;
  connectionId?: string | null;
  name: string;
  sql: string;
  createdAt: number;
  updatedAt: number;
}

export interface Discovered {
  id: string;
  engine: Engine;
  flavor: string;
  version?: string | null;
  host: string;
  port: number;
  source: string;
  container?: string | null;
  image?: string | null;
  username?: string | null;
  password?: string | null;
  database?: string | null;
  authOk?: boolean | null;
  authError?: string | null;
  alreadySaved: boolean;
  /** Handshake detail, e.g. "Password required" or "SQL Server 2022". */
  note?: string | null;
}

export interface Limits {
  connections: number | null;
  history: number | null;
  snippets: number | null;
  tableExport: boolean;
  /** Rows per CSV import (null = unlimited). */
  importRows: number | null;
}

export interface LicenseStatus {
  /** 'launch' = no license needed: every feature is free until `freeUntil`. */
  plan: 'free' | 'launch' | 'pro' | 'team' | string;
  licensed: boolean;
  email?: string | null;
  name?: string | null;
  keyHint?: string | null;
  updatesUntil?: number | null;
  expiresAt?: number | null;
  validateBy?: number | null;
  lastValidated?: number | null;
  message?: string | null;
  deviceId: string;
  limits: Limits;
  /** End of the free launch period (UNIX seconds), while it lasts. */
  freeUntil?: number | null;
}

export interface UpdateInfo {
  version: string;
  notes: string;
  pubDate?: string | null;
  url?: string | null;
  critical: boolean;
  updateAvailable: boolean;
}

export interface AppInfo {
  version: string;
  platform: string;
  arch: string;
  dataDir: string;
  apiBase: string;
  bridge: boolean;
  debug: boolean;
  /** Some passwords are in app.db because the system keychain wasn't available. */
  secretsInFile?: boolean;
  /** app.db was damaged at startup and moved to this path. */
  recoveredDb?: string | null;
}

export interface Settings {
  theme: 'system' | 'light' | 'dark';
  density: 'compact' | 'comfortable';
  editorFontSize: number;
  resultLimit: number;
  pageSize: number;
  telemetry: boolean;
  /** Look for databases on this computer (localhost only) when the app opens. */
  radarOnLaunch: boolean;
  apiBase?: string;
  onboarded?: boolean;
  confirmProdWrites: boolean;
  /** Ask before DELETE/UPDATE without WHERE, DROP, TRUNCATE, ALTER … DROP (any environment). */
  confirmDangerous: boolean;
  showSystemSchemas: boolean;
}

export const DEFAULT_SETTINGS: Settings = {
  theme: 'system',
  density: 'compact',
  editorFontSize: 13,
  resultLimit: 2000,
  pageSize: 300,
  telemetry: false,
  radarOnLaunch: true,
  confirmProdWrites: true,
  confirmDangerous: true,
  showSystemSchemas: false,
};

// ---- AI assistant ---------------------------------------------------------------

export interface AiSettings {
  /** An AiProvider id. */
  provider: string;
  /** Claude model ID. */
  model: string;
  ollamaUrl: string;
  ollamaModel: string;
  /** Chosen model for the other providers, by provider id. */
  models: Record<string, string>;
  /** Base URL for the "custom" OpenAI-compatible provider. */
  customUrl: string;
}

export interface AiProvider {
  id: string;
  name: string;
  wire: 'anthropic' | 'openai' | 'gemini' | 'ollama';
  base: string;
  keysUrl: string;
  needsKey: boolean;
  local: boolean;
}

export interface AiModel {
  id: string;
  name: string;
  note: string;
}

export interface AiInfo {
  settings: AiSettings;
  /** Whether a key is saved, by provider id. */
  keys: Record<string, boolean>;
  providers: AiProvider[];
  /** Free plan: requests per day. null = unlimited. */
  perDay: number | null;
  usedToday: number;
}

export type AiTask = 'generate' | 'explain' | 'fix';

export interface AiJob {
  status: 'thinking' | 'writing' | 'done' | 'error' | 'cancelled';
  sql?: string | null;
  explanation?: string | null;
  error?: string | null;
  model?: string | null;
  stopReason?: string | null;
  inputTokens?: number | null;
  outputTokens?: number | null;
  elapsedMs: number;
}
