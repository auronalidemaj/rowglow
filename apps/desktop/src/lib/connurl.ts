import type { ConnectionConfig, Engine, SslMode } from './types';

export interface ParsedUrl {
  patch: Partial<ConnectionConfig>;
  password?: string;
}

/**
 * Parses connection strings such as
 *   postgres://user:pass@host:5432/db?sslmode=require
 *   mysql://root@127.0.0.1:3306/app
 *   sqlserver://sa@host:1433/db  (also mssql://)
 *   redis://:pass@host:6379/0   rediss:// turns on TLS
 *   mongodb://user:pass@host:27017/db
 *   sqlite:///Users/me/app.db  or a plain path ending in .db / .sqlite / .duckdb
 */
export function parseConnectionUrl(input: string): ParsedUrl | null {
  const raw = input.trim();
  if (!raw) return null;
  if (/^(\/|~\/).+\.(duckdb|ddb)$/i.test(raw)) {
    return { patch: { engine: 'duckdb', filePath: raw } };
  }
  if (/^(\/|~\/).+\.(db|sqlite|sqlite3|db3)$/i.test(raw)) {
    return { patch: { engine: 'sqlite', filePath: raw } };
  }
  // mongodb+srv:// can't be split into host/port: keep the whole URL as the host.
  if (/^mongodb\+srv:\/\//i.test(raw)) {
    try {
      const u = new URL(raw.replace(/^mongodb\+srv:/i, 'http:'));
      const db = decodeURIComponent(u.pathname.replace(/^\//, ''));
      const username = decodeURIComponent(u.username);
      const password = u.password ? decodeURIComponent(u.password) : undefined;
      u.username = '';
      u.password = '';
      return {
        patch: { engine: 'mongodb', host: u.toString().replace(/^http:/, 'mongodb+srv:'), port: 0, username, database: db, sslMode: 'require' },
        password,
      };
    } catch {
      return null;
    }
  }
  const m = /^([a-z][a-z0-9+.-]*):\/\/(.*)$/i.exec(raw);
  if (!m) return null;
  const scheme = m[1].toLowerCase();
  let engine: Engine;
  if (scheme === 'postgres' || scheme === 'postgresql' || scheme === 'pg') engine = 'postgres';
  else if (scheme === 'mysql' || scheme === 'mariadb' || scheme === 'mysql2') engine = 'mysql';
  else if (scheme === 'sqlserver' || scheme === 'mssql') engine = 'mssql';
  else if (scheme === 'redis' || scheme === 'rediss') engine = 'redis';
  else if (scheme === 'mongodb' || scheme === 'mongo') engine = 'mongodb';
  else if (scheme === 'duckdb') {
    const path = decodeURIComponent(m[2].replace(/\?.*$/, ''));
    return { patch: { engine: 'duckdb', filePath: path === ':memory:' || path.startsWith('/') ? path : '/' + path } };
  }
  else if (scheme === 'sqlite' || scheme === 'sqlite3' || scheme === 'file') {
    const path = decodeURIComponent(m[2].replace(/\?.*$/, ''));
    return { patch: { engine: 'sqlite', filePath: path.startsWith('/') ? path : '/' + path } };
  } else return null;

  let url: URL;
  try {
    url = new URL(`http://${m[2]}`);
  } catch {
    return null;
  }
  const patch: Partial<ConnectionConfig> = { engine };
  if (url.hostname) patch.host = decodeURIComponent(url.hostname).replace(/^\[|\]$/g, '');
  if (url.port) patch.port = Number(url.port);
  if (url.username) patch.username = decodeURIComponent(url.username);
  const db = decodeURIComponent(url.pathname.replace(/^\//, ''));
  if (db) patch.database = db;
  if (scheme === 'rediss') patch.sslMode = 'require';
  if (engine === 'mongodb') {
    const authSource = url.searchParams.get('authSource');
    if (authSource && !db) patch.database = authSource;
    if (url.searchParams.get('tls') === 'true' || url.searchParams.get('ssl') === 'true') patch.sslMode = 'require';
  }
  // sqlserver://host;databaseName=x style (JDBC) isn't a URL; only the URL form is handled.
  const ssl = url.searchParams.get('sslmode') ?? url.searchParams.get('ssl-mode') ?? url.searchParams.get('ssl');
  if (ssl) {
    const s = ssl.toLowerCase();
    const mode: SslMode = s === 'disable' || s === 'false' || s === '0' ? 'disable' : s === 'prefer' || s === 'allow' ? 'prefer' : 'require';
    patch.sslMode = mode;
  }
  const hostParam = url.searchParams.get('host');
  if (hostParam && hostParam.startsWith('/')) patch.host = hostParam;
  return { patch, password: url.password ? decodeURIComponent(url.password) : undefined };
}
