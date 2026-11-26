import type { Engine } from './types';

export function quoteIdent(engine: Engine, name: string): string {
  if (engine === 'mysql') return '`' + name.replace(/`/g, '``') + '`';
  if (engine === 'mssql') return '[' + name.replace(/]/g, ']]') + ']';
  return '"' + name.replace(/"/g, '""') + '"';
}

export function quoteLiteral(engine: Engine, value: string): string {
  if (engine === 'mysql') {
    return "'" + value.replace(/\\/g, '\\\\').replace(/'/g, "''").replace(/\0/g, '\\0') + "'";
  }
  // N'…' keeps non-ASCII text intact in nvarchar columns.
  if (engine === 'mssql') return "N'" + value.replace(/'/g, "''") + "'";
  return "'" + value.replace(/'/g, "''") + "'";
}

export function qualified(engine: Engine, schema: string, name: string): string {
  if (!schema || (engine === 'sqlite' && schema === 'main')) return quoteIdent(engine, name);
  return `${quoteIdent(engine, schema)}.${quoteIdent(engine, name)}`;
}

export function isNumericType(t: string | undefined): boolean {
  if (!t) return false;
  const s = t.toLowerCase();
  if (s.includes('interval') || s.includes('point') || s.startsWith('_') || s.endsWith('[]')) return false;
  return /(int|numeric|decimal|float|double|real|serial|money|year|oid)/.test(s) && !s.includes('point');
}

export function isBoolType(t: string | undefined): boolean {
  if (!t) return false;
  const s = t.toLowerCase();
  return s === 'bool' || s === 'boolean' || s === 'tinyint(1)';
}

export function isJsonType(t: string | undefined): boolean {
  if (!t) return false;
  const s = t.toLowerCase();
  return s === 'json' || s === 'jsonb';
}

export function isBinaryType(t: string | undefined): boolean {
  if (!t) return false;
  const s = t.toLowerCase();
  return s === 'bytea' || s.includes('blob') || s.includes('binary') || s === 'geometry' || s === 'image';
}

/** Columns the database computes itself (generated / identity ALWAYS) can't be written. */
export function isComputedColumn(extra: string | null | undefined): boolean {
  return !!extra && /(generated|identity always|^identity\()/i.test(extra);
}

export function isBitType(t: string | undefined): boolean {
  return !!t && /^bit(\(|$)/i.test(t);
}

const NUMBER = /^-?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/;

/** SQL literal for a cell value, typed by column type. */
export function sqlValue(engine: Engine, value: string | null, typeName?: string): string {
  if (value === null) return 'NULL';
  // Binary values are displayed as hex literals; send them back the same way.
  if (engine === 'mysql' && isBinaryType(typeName) && /^0x[0-9A-Fa-f]*$/.test(value)) return `X'${value.slice(2)}'`;
  if (engine === 'sqlite' && isBinaryType(typeName) && /^x'[0-9A-Fa-f]*'$/.test(value)) return `X'${value.slice(2, -1)}'`;
  if (engine === 'mssql' && isBinaryType(typeName) && /^0x[0-9A-Fa-f]*$/.test(value)) return value;
  if (engine === 'duckdb' && isBinaryType(typeName) && /^0x[0-9A-Fa-f]*$/.test(value)) return `from_hex('${value.slice(2)}')`;
  if ((isNumericType(typeName) || isBitType(typeName)) && NUMBER.test(value.trim())) return value.trim();
  if (isBoolType(typeName) && engine !== 'mysql' && /^(true|false)$/i.test(value.trim())) return value.trim().toUpperCase();
  return quoteLiteral(engine, value);
}

/** Engines whose server holds several databases you can switch between in the sidebar. */
export function switchesDatabases(engine: Engine): boolean {
  return engine === 'postgres' || engine === 'mssql';
}

export const ENGINE_LABELS: Record<Engine, string> = {
  postgres: 'PostgreSQL',
  mysql: 'MySQL',
  sqlite: 'SQLite',
  mssql: 'SQL Server',
  duckdb: 'DuckDB',
  redis: 'Redis',
  mongodb: 'MongoDB',
};

export function engineLabel(engine: Engine, flavor?: string): string {
  return flavor || ENGINE_LABELS[engine] || engine;
}

export function defaultPort(engine: Engine): number {
  switch (engine) {
    case 'postgres':
      return 5432;
    case 'mysql':
      return 3306;
    case 'mssql':
      return 1433;
    case 'redis':
      return 6379;
    case 'mongodb':
      return 27017;
    default:
      return 0;
  }
}
