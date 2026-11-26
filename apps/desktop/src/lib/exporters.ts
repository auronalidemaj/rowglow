import { isBoolType, isJsonType, isNumericType, qualified, quoteIdent, sqlValue } from './dialect';
import type { Cell, Column, Engine } from './types';

function csvField(v: Cell, sep: string): string {
  if (v === null) return '';
  if (v.includes(sep) || v.includes('"') || v.includes('\n') || v.includes('\r')) {
    return '"' + v.replace(/"/g, '""') + '"';
  }
  return v;
}

export function toDelimited(columns: Column[], rows: Cell[][], sep = ',', header = true): string {
  const lines: string[] = [];
  if (header) lines.push(columns.map((c) => csvField(c.name, sep)).join(sep));
  for (const r of rows) lines.push(r.map((v) => csvField(v, sep)).join(sep));
  return lines.join('\n');
}

/** TSV for clipboard: tabs/newlines inside values are flattened (spreadsheet friendly). */
export function toTSV(columns: Column[] | null, rows: Cell[][]): string {
  const clean = (v: Cell) => (v === null ? '' : v.replace(/[\t\r\n]+/g, ' '));
  const lines: string[] = [];
  if (columns) lines.push(columns.map((c) => c.name).join('\t'));
  for (const r of rows) lines.push(r.map(clean).join('\t'));
  return lines.join('\n');
}

function jsonValue(v: Cell, type: string): unknown {
  if (v === null) return null;
  if (isNumericType(type) && v.trim() !== '' && !Number.isNaN(Number(v))) {
    const n = Number(v);
    if (Number.isSafeInteger(n) || !/^-?\d+$/.test(v)) return n;
  }
  if (isBoolType(type)) {
    if (v === 't' || v === 'true' || v === '1') return true;
    if (v === 'f' || v === 'false' || v === '0') return false;
  }
  if (isJsonType(type)) {
    try {
      return JSON.parse(v);
    } catch {
      /* keep string */
    }
  }
  return v;
}

export function toJSON(columns: Column[], rows: Cell[][]): string {
  const arr = rows.map((r) => {
    const o: Record<string, unknown> = {};
    columns.forEach((c, i) => (o[c.name] = jsonValue(r[i], c.typeName)));
    return o;
  });
  return JSON.stringify(arr, null, 2);
}

export function toInserts(engine: Engine, schema: string, table: string, columns: Column[], rows: Cell[][]): string {
  const target = table ? qualified(engine, schema, table) : quoteIdent(engine, 'table_name');
  const cols = columns.map((c) => quoteIdent(engine, c.name)).join(', ');
  return rows
    .map((r) => `INSERT INTO ${target} (${cols}) VALUES (${r.map((v, i) => sqlValue(engine, v, columns[i]?.typeName)).join(', ')});`)
    .join('\n');
}

export function toMarkdown(columns: Column[], rows: Cell[][]): string {
  const esc = (v: Cell) => (v === null ? 'NULL' : v.replace(/\|/g, '\\|').replace(/\r?\n/g, ' '));
  const head = `| ${columns.map((c) => esc(c.name)).join(' | ')} |`;
  const sep = `| ${columns.map(() => '---').join(' | ')} |`;
  return [head, sep, ...rows.map((r) => `| ${r.map(esc).join(' | ')} |`)].join('\n');
}
