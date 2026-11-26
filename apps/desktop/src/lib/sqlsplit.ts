import type { Engine } from './types';

export interface Stmt {
  /** Statement text without the trailing delimiter, trimmed. */
  text: string;
  /** Offset of the first character of the statement (after leading whitespace/comments). */
  from: number;
  /** Offset just past the statement's last character (before the delimiter). */
  to: number;
  /** Offset just past the delimiter (or `to` if none). */
  end: number;
}

const BLOCK_START = /^\s*CREATE\s+(OR\s+(REPLACE|ALTER)\s+)?(DEFINER\s*=\s*\S+\s+)?((TEMP|TEMPORARY)\s+)?(TRIGGER|PROCEDURE|FUNCTION|EVENT)\b/i;

/**
 * Splits a SQL script into statements, respecting quotes, comments,
 * Postgres dollar-quoting / E-strings, MySQL backslash escapes, `#` comments
 * and `DELIMITER` directives, T-SQL `GO` batch separators and [bracketed]
 * names, and BEGIN…END bodies of triggers/procedures.
 */
export function splitStatements(sql: string, engine: Engine): Stmt[] {
  const out: Stmt[] = [];
  const n = sql.length;
  let i = 0;
  let stmtStart = -1; // offset of first significant char of current statement
  let delimiter = ';';
  let depth = 0;
  let blockMode = false;
  let pgAtomic = false;
  const words: string[] = []; // recent words of current statement (uppercased)

  const isWordChar = (c: string) => /[A-Za-z0-9_$]/.test(c);

  const push = (to: number, end: number) => {
    if (stmtStart >= 0) {
      let t = to;
      while (t > stmtStart && /\s/.test(sql[t - 1])) t--;
      const text = sql.slice(stmtStart, t);
      if (text.trim()) out.push({ text, from: stmtStart, to: t, end });
    }
    stmtStart = -1;
    depth = 0;
    blockMode = false;
    pgAtomic = false;
    words.length = 0;
  };

  const markStart = (pos: number) => {
    if (stmtStart < 0) stmtStart = pos;
  };

  while (i < n) {
    const c = sql[i];
    const next = sql[i + 1];

    // T-SQL `GO` on a line of its own ends the batch (sqlcmd / SSMS convention).
    if (engine === 'mssql' && (c === 'G' || c === 'g') && (i === 0 || /\n[ \t]*$/.test(sql.slice(Math.max(0, i - 200), i)) || /^[ \t]*$/.test(sql.slice(0, i)))) {
      const lineEnd = sql.indexOf('\n', i);
      const line = sql.slice(i, lineEnd < 0 ? n : lineEnd);
      if (/^GO(\s+\d+)?\s*(--.*)?$/i.test(line.trim())) {
        if (stmtStart >= 0) push(i, i);
        i = lineEnd < 0 ? n : lineEnd + 1;
        continue;
      }
    }

    // MySQL client DELIMITER directive (only at statement start).
    if (engine === 'mysql' && stmtStart < 0 && /^DELIMITER\s/i.test(sql.slice(i, i + 10))) {
      const lineEnd = sql.indexOf('\n', i);
      const line = sql.slice(i, lineEnd < 0 ? n : lineEnd).trim();
      const d = line.split(/\s+/)[1];
      if (d) delimiter = d;
      i = lineEnd < 0 ? n : lineEnd + 1;
      continue;
    }

    // Whitespace
    if (/\s/.test(c)) {
      i++;
      continue;
    }

    // Line comments: -- (and # for MySQL)
    if ((c === '-' && next === '-') || (engine === 'mysql' && c === '#')) {
      const lineEnd = sql.indexOf('\n', i);
      i = lineEnd < 0 ? n : lineEnd + 1;
      continue;
    }

    // Block comments (nested in Postgres)
    if (c === '/' && next === '*') {
      let d = 1;
      i += 2;
      while (i < n && d > 0) {
        if (sql[i] === '*' && sql[i + 1] === '/') {
          d--;
          i += 2;
        } else if (engine === 'postgres' && sql[i] === '/' && sql[i + 1] === '*') {
          d++;
          i += 2;
        } else i++;
      }
      continue;
    }

    // Delimiter
    if (sql.startsWith(delimiter, i) && !(blockMode && depth > 0 && delimiter === ';')) {
      if (stmtStart >= 0) push(i, i + delimiter.length);
      i += delimiter.length;
      continue;
    }

    markStart(i);

    // Single-quoted strings
    if (c === "'") {
      const backslash =
        engine === 'mysql' ||
        (engine === 'postgres' && i > 0 && /[eE]/.test(sql[i - 1]) && (i < 2 || !isWordChar(sql[i - 2])));
      i++;
      while (i < n) {
        if (backslash && sql[i] === '\\') {
          i += 2;
          continue;
        }
        if (sql[i] === "'") {
          if (sql[i + 1] === "'") {
            i += 2;
            continue;
          }
          i++;
          break;
        }
        i++;
      }
      continue;
    }

    // Quoted identifiers / MySQL double-quoted strings / backticks
    if (c === '"' || c === '`' || (c === '[' && (engine === 'sqlite' || engine === 'mssql'))) {
      const close = c === '[' ? ']' : c;
      const backslash = engine === 'mysql' && c === '"';
      i++;
      while (i < n) {
        if (backslash && sql[i] === '\\') {
          i += 2;
          continue;
        }
        if (sql[i] === close) {
          if (sql[i + 1] === close && close !== ']') {
            i += 2;
            continue;
          }
          i++;
          break;
        }
        i++;
      }
      continue;
    }

    // Postgres dollar-quoted strings: $$...$$ or $tag$...$tag$
    if (c === '$' && (engine === 'postgres' || engine === 'duckdb') && (i === 0 || !isWordChar(sql[i - 1]))) {
      const m = /^\$([A-Za-z_][A-Za-z0-9_]*)?\$/.exec(sql.slice(i, i + 64));
      if (m) {
        const tag = m[0];
        const close = sql.indexOf(tag, i + tag.length);
        i = close < 0 ? n : close + tag.length;
        continue;
      }
    }

    // Words (keywords / identifiers)
    if (isWordChar(c)) {
      let j = i;
      while (j < n && isWordChar(sql[j]) && !(delimiter !== ';' && j > i && sql.startsWith(delimiter, j))) j++;
      const w = sql.slice(i, j).toUpperCase();
      words.push(w);
      if (words.length === 1 || (words.length <= 8 && !blockMode)) {
        if (engine !== 'postgres' && BLOCK_START.test(sql.slice(stmtStart, j + 1))) blockMode = true;
      }
      if (engine === 'postgres' && w === 'ATOMIC' && words[words.length - 2] === 'BEGIN') {
        blockMode = true;
        pgAtomic = true;
        depth++;
      } else if (blockMode && !pgAtomic) {
        if (w === 'CASE') depth++;
        else if (w === 'BEGIN') {
          // T-SQL BEGIN TRAN / BEGIN TRANSACTION isn't a block.
          const rest = /^\s*([A-Za-z]+)/.exec(sql.slice(j, j + 20));
          const nw = rest ? rest[1].toUpperCase() : '';
          if (!['TRAN', 'TRANSACTION', 'DISTRIBUTED'].includes(nw)) depth++;
        } else if (w === 'END') {
          // Peek the next word: END IF / END LOOP / END WHILE / END REPEAT don't close BEGIN/CASE.
          const rest = /^\s*([A-Za-z]+)/.exec(sql.slice(j, j + 20));
          const nw = rest ? rest[1].toUpperCase() : '';
          if (!['IF', 'LOOP', 'WHILE', 'REPEAT'].includes(nw)) depth = Math.max(0, depth - 1);
        }
      } else if (pgAtomic) {
        if (w === 'CASE') depth++;
        else if (w === 'END') depth = Math.max(0, depth - 1);
      }
      i = j;
      continue;
    }

    i++;
  }
  if (stmtStart >= 0) push(n, n);
  return out;
}

/**
 * Returns the statement under the cursor. The cursor may also sit on the same
 * line just after a statement's delimiter. On a blank line between statements
 * nothing is returned, so a stray ⌘↵ never runs the statement above by accident.
 */
export function statementAt(stmts: Stmt[], pos: number, text?: string): Stmt | null {
  if (!stmts.length) return null;
  for (const s of stmts) {
    if (pos >= s.from && pos <= s.end) return s;
  }
  if (text === undefined) return null;
  // Same line, after the delimiter (e.g. "select 1;  |")
  for (let i = stmts.length - 1; i >= 0; i--) {
    const s = stmts[i];
    if (s.end <= pos && !text.slice(s.end, pos).includes('\n')) return s;
  }
  // Same line, before the statement starts (e.g. "  |select 1")
  for (const s of stmts) {
    if (s.from >= pos && !text.slice(pos, s.from).includes('\n')) return s;
  }
  return null;
}

/** Removes comments and string contents so keyword checks only see real SQL. */
export function stripSql(sql: string): string {
  return sql
    .replace(/--[^\n]*/g, ' ')
    .replace(/\/\*[\s\S]*?\*\//g, ' ')
    .replace(/\$([A-Za-z_][A-Za-z0-9_]*)?\$[\s\S]*?\$\1\$/g, "''")
    .replace(/'(?:[^'\\]|''|\\.)*'/g, "''")
    .replace(/"(?:[^"]|"")*"/g, 'x')
    .replace(/`(?:[^`]|``)*`/g, 'x')
    .replace(/\[(?:[^\]]|\]\])*\]/g, 'x');
}

/**
 * Statements that can destroy a lot of data in one go. Returns a short reason,
 * or null if the statement is not considered dangerous.
 */
export function dangerousReason(sql: string, engine?: Engine): string | null {
  const s = stripSql(sql).trim().replace(/\s+/g, ' ');
  let first = (/^[(\s]*([A-Za-z]+)/.exec(s)?.[1] ?? '').toUpperCase();
  // A T-SQL batch can hold several statements without semicolons: judge the
  // most destructive one it contains.
  if (engine === 'mssql' && !['DELETE', 'UPDATE', 'TRUNCATE', 'DROP'].includes(first)) {
    const m = /\b(TRUNCATE|DROP|DELETE|UPDATE)\b/i.exec(s);
    if (m) {
      const tail = s.slice(m.index);
      return dangerousReason(tail, 'mysql') ?? null;
    }
    first = first || '';
  }
  const target = (re: RegExp) => re.exec(s)?.[1] ?? '';
  if (first === 'DELETE' && !/\bWHERE\b/i.test(s)) return `DELETE without WHERE removes every row in ${target(/^DELETE\s+(?:FROM\s+)?([\w."`\[\]]+)/i) || 'the table'}`;
  if (first === 'UPDATE' && !/\bWHERE\b/i.test(s)) return `UPDATE without WHERE changes every row in ${target(/^UPDATE\s+(?:ONLY\s+)?([\w."`\[\]]+)/i) || 'the table'}`;
  if (first === 'TRUNCATE') return 'TRUNCATE removes every row';
  if (first === 'DROP') return `DROP ${target(/^DROP\s+(\w+(?:\s+VIEW)?)/i).toUpperCase() || 'object'} deletes it permanently`;
  if (first === 'ALTER' && /\bDROP\b/i.test(s)) return 'ALTER … DROP permanently removes a column or constraint';
  return null;
}

const READ_ONLY = new Set(['SELECT', 'FROM', 'SUMMARIZE', 'SHOW', 'EXPLAIN', 'DESCRIBE', 'DESC', 'VALUES', 'TABLE', 'PRAGMA', 'USE', 'SET', 'BEGIN', 'START', 'COMMIT', 'ROLLBACK', 'END', 'SAVEPOINT', 'RELEASE']);

const TSQL_WRITES = /\b(INSERT|UPDATE|DELETE|MERGE|TRUNCATE|DROP|CREATE|ALTER|INTO|EXEC|EXECUTE|GRANT|REVOKE|DENY|BULK|BACKUP|RESTORE|DBCC|KILL|SHUTDOWN|RECONFIGURE)\b/i;

/** Heuristic: does the statement modify data or schema? */
export function isWriteStatement(sql: string, engine?: Engine): boolean {
  if (engine === 'mssql') return TSQL_WRITES.test(stripSql(sql));
  const clean = sql
    .replace(/--[^\n]*/g, ' ')
    .replace(/\/\*[\s\S]*?\*\//g, ' ')
    .replace(/'(?:[^']|'')*'/g, "''")
    .trim();
  const first = (/^[(\s]*([A-Za-z]+)/.exec(clean)?.[1] ?? '').toUpperCase();
  if (first === 'WITH') return /\b(INSERT|UPDATE|DELETE|MERGE)\b/i.test(clean);
  if (first === 'EXPLAIN') return /\bANALY[SZ]E\b/i.test(clean) && /\b(INSERT|UPDATE|DELETE|MERGE)\b/i.test(clean);
  if (first === 'SELECT') return /\bINTO\s+(?!@)/i.test(clean);
  return !READ_ONLY.has(first);
}
