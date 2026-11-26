import { qualified, quoteIdent, quoteLiteral, sqlValue } from './dialect';
import type { Cell, Engine } from './types';

export type FilterOp =
  | '='
  | '<>'
  | '<'
  | '>'
  | '<='
  | '>='
  | 'contains'
  | 'not contains'
  | 'starts with'
  | 'ends with'
  | 'in'
  | 'is null'
  | 'is not null';

export const FILTER_OPS: FilterOp[] = [
  '=',
  '<>',
  '<',
  '>',
  '<=',
  '>=',
  'contains',
  'not contains',
  'starts with',
  'ends with',
  'in',
  'is null',
  'is not null',
];

export interface Filter {
  id: string;
  column: string; // '*' = any column
  op: FilterOp;
  value: string;
  enabled: boolean;
}

export interface Sort {
  column: string;
  dir: 'asc' | 'desc';
}

export interface ColMeta {
  name: string;
  type: string;
}

function asText(engine: Engine, col: string): string {
  const q = quoteIdent(engine, col);
  if (engine === 'postgres') return `CAST(${q} AS text)`;
  if (engine === 'mysql') return `CAST(${q} AS CHAR)`;
  if (engine === 'mssql') return `CAST(${q} AS NVARCHAR(MAX))`;
  if (engine === 'duckdb') return `CAST(${q} AS VARCHAR)`;
  return `CAST(${q} AS TEXT)`;
}

function likeEscape(v: string): string {
  return v.replace(/[\\%_]/g, (m) => '\\' + m);
}

function likeExpr(engine: Engine, col: string, pattern: string, negate = false): string {
  const ci = engine === 'postgres' || engine === 'duckdb';
  const op = ci ? (negate ? 'NOT ILIKE' : 'ILIKE') : negate ? 'NOT LIKE' : 'LIKE';
  return `${asText(engine, col)} ${op} ${quoteLiteral(engine, pattern)} ESCAPE ${quoteLiteral(engine, '\\')}`;
}

function oneFilter(engine: Engine, f: Filter, columns: ColMeta[]): string | null {
  const meta = columns.find((c) => c.name === f.column);
  const col = quoteIdent(engine, f.column);
  const lit = (v: string) => sqlValue(engine, v, meta?.type);
  const v = f.value;
  switch (f.op) {
    case 'is null':
      return `${col} IS NULL`;
    case 'is not null':
      return `${col} IS NOT NULL`;
    case 'contains':
      return likeExpr(engine, f.column, `%${likeEscape(v)}%`);
    case 'not contains':
      return likeExpr(engine, f.column, `%${likeEscape(v)}%`, true);
    case 'starts with':
      return likeExpr(engine, f.column, `${likeEscape(v)}%`);
    case 'ends with':
      return likeExpr(engine, f.column, `%${likeEscape(v)}`);
    case 'in': {
      const items = v
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean);
      if (!items.length) return null;
      return `${col} IN (${items.map(lit).join(', ')})`;
    }
    default:
      return `${col} ${f.op} ${lit(v)}`;
  }
}

export function buildWhere(engine: Engine, filters: Filter[], columns: ColMeta[], rawWhere?: string): string {
  if (rawWhere !== undefined) {
    const raw = rawWhere.trim().replace(/^where\s+/i, '');
    return raw ? `WHERE ${raw}` : '';
  }
  const parts: string[] = [];
  for (const f of filters) {
    if (!f.enabled || !f.column) continue;
    const needsValue = !['is null', 'is not null'].includes(f.op);
    if (needsValue && f.value === '') continue;
    if (f.column === '*') {
      const any = columns
        .map((c) => likeExpr(engine, c.name, `%${likeEscape(f.value)}%`, f.op === 'not contains'))
        .join(f.op === 'not contains' ? ' AND ' : ' OR ');
      if (any) parts.push(`(${any})`);
      continue;
    }
    const p = oneFilter(engine, f, columns);
    if (p) parts.push(p);
  }
  return parts.length ? `WHERE ${parts.join(' AND ')}` : '';
}

export function buildOrderBy(engine: Engine, sort: Sort[], fallback: string[]): string {
  const cols = sort.length
    ? sort.map((s) => `${quoteIdent(engine, s.column)} ${s.dir.toUpperCase()}`)
    : fallback.map((c) => quoteIdent(engine, c));
  return cols.length ? `ORDER BY ${cols.join(', ')}` : '';
}

export function buildSelect(
  engine: Engine,
  schema: string,
  table: string,
  opts: { where: string; orderBy: string; limit?: number; offset?: number },
): string {
  const parts = [`SELECT * FROM ${qualified(engine, schema, table)}`];
  if (opts.where) parts.push(opts.where);
  if (engine === 'mssql') {
    // T-SQL pages with OFFSET … FETCH, which needs an ORDER BY.
    if (opts.limit !== undefined || opts.offset) {
      parts.push(opts.orderBy || 'ORDER BY (SELECT NULL)');
      parts.push(`OFFSET ${opts.offset ?? 0} ROWS`);
      if (opts.limit !== undefined) parts.push(`FETCH NEXT ${opts.limit} ROWS ONLY`);
    } else if (opts.orderBy) {
      parts.push(opts.orderBy);
    }
    return parts.join(' ');
  }
  if (opts.orderBy) parts.push(opts.orderBy);
  if (opts.limit !== undefined) parts.push(`LIMIT ${opts.limit}`);
  if (opts.offset) parts.push(`OFFSET ${opts.offset}`);
  return parts.join(' ');
}

export function buildCount(engine: Engine, schema: string, table: string, where: string): string {
  return `SELECT count(*) FROM ${qualified(engine, schema, table)}${where ? ' ' + where : ''}`;
}

/** Staged edits for one table grid. */
export interface StagedChanges {
  /** rowIndex -> column name -> new value */
  updates: Map<number, Map<string, Cell>>;
  /** rows to delete (by index into the loaded rows) */
  deletes: Set<number>;
  /** new rows: column name -> value. Missing columns use DEFAULT. */
  inserts: { key: string; values: Map<string, Cell> }[];
}

export function emptyChanges(): StagedChanges {
  return { updates: new Map(), deletes: new Set(), inserts: [] };
}

export function changeCount(c: StagedChanges): number {
  let n = c.deletes.size + c.inserts.length;
  c.updates.forEach((cols, row) => {
    if (!c.deletes.has(row) && cols.size) n++;
  });
  return n;
}

export class NoKeyError extends Error {}

/**
 * Turns staged edits into SQL statements. Rows are identified by primary key
 * values as originally loaded (so editing a PK column still targets the right row).
 */
export function buildChanges(
  engine: Engine,
  schema: string,
  table: string,
  columns: ColMeta[],
  keyColumns: string[],
  rows: Cell[][],
  changes: StagedChanges,
): string[] {
  const target = qualified(engine, schema, table);
  const idx = new Map(columns.map((c, i) => [c.name, i]));
  const type = (name: string) => columns[idx.get(name) ?? -1]?.type;
  const whereFor = (row: Cell[]) => {
    if (!keyColumns.length) throw new NoKeyError('This table has no primary key, so rows can’t be identified safely.');
    return keyColumns
      .map((k) => {
        const v = row[idx.get(k)!];
        return v === null ? `${quoteIdent(engine, k)} IS NULL` : `${quoteIdent(engine, k)} = ${sqlValue(engine, v, type(k))}`;
      })
      .join(' AND ');
  };
  const out: string[] = [];
  const updateRows = [...changes.updates.keys()].sort((a, b) => a - b);
  for (const r of updateRows) {
    if (changes.deletes.has(r)) continue;
    const cols = changes.updates.get(r)!;
    if (!cols.size) continue;
    const sets = [...cols.entries()]
      .map(([name, v]) => `${quoteIdent(engine, name)} = ${sqlValue(engine, v, type(name))}`)
      .join(', ');
    out.push(`UPDATE ${target} SET ${sets} WHERE ${whereFor(rows[r])}`);
  }
  for (const r of [...changes.deletes].sort((a, b) => a - b)) {
    out.push(`DELETE FROM ${target} WHERE ${whereFor(rows[r])}`);
  }
  for (const ins of changes.inserts) {
    const entries = [...ins.values.entries()];
    if (!entries.length) {
      out.push(engine === 'mysql' ? `INSERT INTO ${target} () VALUES ()` : `INSERT INTO ${target} DEFAULT VALUES`);
      continue;
    }
    const cols = entries.map(([n]) => quoteIdent(engine, n)).join(', ');
    const vals = entries.map(([n, v]) => sqlValue(engine, v, type(n))).join(', ');
    out.push(`INSERT INTO ${target} (${cols}) VALUES (${vals})`);
  }
  return out;
}
