/**
 * End-to-end write tests: builds staged-edit SQL exactly like the app does
 * (sqlgen.buildChanges), runs it through the real Rust core via the dev bridge
 * against real Postgres / MySQL / SQLite, and reads the data back.
 *
 * Needs: `scripts/seed-demo.sh` databases and a running bridge:
 *   cargo run --manifest-path src-tauri/Cargo.toml --features bridge --bin rowglow-bridge
 *   ROWGLOW_E2E=1 pnpm vitest run src/lib/edits.e2e.test.ts
 */
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, describe, expect, it } from 'vitest';
import { buildChanges, emptyChanges, type ColMeta } from './sqlgen';
import type { Cell, Engine, StatementOutcome, TableStructure } from './types';

const RUN = !!process.env.ROWGLOW_E2E;
const BRIDGE = 'http://127.0.0.1:1425/rpc';

async function rpc<T>(method: string, params: Record<string, unknown> = {}): Promise<T> {
  const res = await fetch(BRIDGE, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ method, params }) });
  const body = await res.json();
  if (body.error) throw Object.assign(new Error(body.error.message), body.error);
  return body.ok as T;
}

const TRICKY_NAME = `O'Brien "the \\ backslash" ; -- not a comment`;
const TRICKY_NOTE = 'multi\nline\ttab 🎉 ünïcødé 中文';
const WILD = '%_wild_% \\n literal';
const JSON_VALUE = '{"a": [1, "x\\\\y", null], "b": "O\'Brien"}';

interface Target {
  engine: Engine;
  connection: Record<string, unknown>;
  schema: string;
  create: string;
  trueValue: string;
  bin: { write: string; read: string; type: string };
}

const sqlitePath = join(tmpdir(), `rowglow-e2e-${Date.now()}.sqlite`);

const TARGETS: Target[] = [
  {
    engine: 'postgres',
    connection: { engine: 'postgres', host: '127.0.0.1', port: 5432, username: process.env.USER, database: 'rowglow_demo' },
    schema: 'public',
    create: `CREATE TABLE public.rowglow_e2e (id int PRIMARY KEY, name text, note text, amount numeric(10,2), flag boolean, data jsonb, made date, bin bytea, total int GENERATED ALWAYS AS (id * 10) STORED)`,
    trueValue: 'true',
    bin: { write: '\\xa1b2', read: '\\xa1b2', type: 'bytea' },
  },
  {
    engine: 'mysql',
    connection: { engine: 'mysql', host: '127.0.0.1', port: 3306, username: 'root', database: 'rowglow_demo' },
    schema: 'rowglow_demo',
    create: `CREATE TABLE rowglow_e2e (id INT PRIMARY KEY, name VARCHAR(200), note TEXT, amount DECIMAL(10,2), flag TINYINT(1), data JSON, made DATE, bin VARBINARY(16)) CHARACTER SET utf8mb4`,
    trueValue: '1',
    bin: { write: '0xA1B2', read: '0xA1B2', type: 'varbinary(16)' },
  },
  {
    engine: 'sqlite',
    connection: { engine: 'sqlite', filePath: sqlitePath },
    schema: 'main',
    create: `CREATE TABLE rowglow_e2e (id INTEGER PRIMARY KEY, name TEXT, note TEXT, amount REAL, flag INTEGER, data TEXT, made TEXT, bin BLOB)`,
    trueValue: '1',
    bin: { write: "x'A1B2'", read: "x'A1B2'", type: 'blob' },
  },
];

const cleanup: (() => Promise<unknown>)[] = [];
afterAll(async () => {
  for (const f of cleanup.reverse()) await f().catch(() => {});
});

describe.skipIf(!RUN).each(TARGETS)('staged edits on $engine', (t) => {
  let sessionId = '';
  let connectionId = '';
  let cols: ColMeta[] = [];
  const exec = async (sql: string, channel: 'aux' | 'editor' = 'aux') => {
    const [o] = await rpc<StatementOutcome[]>('query.execute', { sessionId, statements: [sql], channel, limit: 1000 });
    return o;
  };
  const rowsNow = async () => {
    const o = await exec('SELECT * FROM rowglow_e2e ORDER BY id');
    expect(o.error).toBeUndefined();
    return { columns: o.result!.columns.map((c) => c.name), rows: o.result!.rows };
  };
  const apply = (statements: string[]) =>
    rpc<{ ok: boolean; failedIndex?: number; error?: { message: string; code?: string } }>('query.transaction', {
      sessionId,
      statements,
      expectOneRow: true,
    });

  it('connects and creates a scratch table', async () => {
    if (t.engine === 'sqlite') await rpc('connections.createSqlite', { path: sqlitePath });
    const saved = await rpc<{ id: string }>('connections.save', {
      connection: { name: `e2e ${t.engine}`, ...t.connection },
    });
    connectionId = saved.id;
    cleanup.push(() => rpc('connections.delete', { id: saved.id }));
    const info = await rpc<{ sessionId: string }>('session.open', { connectionId: saved.id });
    sessionId = info.sessionId;
    await exec('DROP TABLE IF EXISTS rowglow_e2e', 'editor');
    const created = await exec(t.create, 'editor');
    expect(created.error).toBeUndefined();
    cleanup.push(() => exec('DROP TABLE IF EXISTS rowglow_e2e', 'editor'));
    const st = await rpc<TableStructure>('meta.structure', { sessionId, schema: t.schema, table: 'rowglow_e2e' });
    expect(st.primaryKey).toEqual(['id']);
    cols = st.columns.map((c) => ({ name: c.name, type: c.dataType }));
  });

  it('inserts tricky values and reads them back exactly', async () => {
    const ch = emptyChanges();
    const row = (v: Record<string, Cell>) => ({ key: String(v.id), values: new Map(Object.entries(v)) });
    ch.inserts.push(
      row({ id: '1', name: TRICKY_NAME, note: TRICKY_NOTE, amount: '12.50', flag: t.trueValue, data: JSON_VALUE, made: '2026-01-31', bin: t.bin.write }),
      row({ id: '2', name: WILD, note: '', amount: '0', flag: null, data: null, made: null, bin: null }),
      row({ id: '3', name: 'third' }),
    );
    const stmts = buildChanges(t.engine, t.schema, 'rowglow_e2e', cols, ['id'], [], ch);
    const res = await apply(stmts);
    expect(res).toMatchObject({ ok: true });
    const { columns, rows } = await rowsNow();
    const get = (r: number, c: string) => rows[r][columns.indexOf(c)];
    expect(rows).toHaveLength(3);
    expect(get(0, 'name')).toBe(TRICKY_NAME);
    expect(get(0, 'note')).toBe(TRICKY_NOTE);
    expect(Number(get(0, 'amount'))).toBe(12.5);
    expect(['t', 'true', '1']).toContain(get(0, 'flag'));
    expect(JSON.parse(get(0, 'data')!)).toEqual(JSON.parse(JSON_VALUE));
    expect(get(0, 'made')).toBe('2026-01-31');
    expect(get(0, 'bin')).toBe(t.bin.read);
    expect(get(1, 'name')).toBe(WILD);
    expect(get(1, 'note')).toBe(''); // empty string stays empty, not NULL
    expect(get(1, 'data')).toBeNull();
    expect(get(2, 'note')).toBeNull(); // untouched column → DEFAULT (NULL)
  });

  it('updates by primary key using the loaded rows (incl. setting NULL)', async () => {
    const { rows } = await rowsNow();
    const ch = emptyChanges();
    ch.updates.set(0, new Map<string, Cell>([['note', null], ['name', 'Renamed \\ again']]));
    ch.updates.set(1, new Map<string, Cell>([['amount', '0.10']]));
    const res = await apply(buildChanges(t.engine, t.schema, 'rowglow_e2e', cols, ['id'], rows, ch));
    expect(res).toMatchObject({ ok: true });
    const after = await rowsNow();
    const get = (r: number, c: string) => after.rows[r][after.columns.indexOf(c)];
    expect(get(0, 'note')).toBeNull();
    expect(get(0, 'name')).toBe('Renamed \\ again');
    expect(Number(get(1, 'amount'))).toBe(0.1);
    expect(get(2, 'name')).toBe('third'); // other rows untouched
  });

  it('accepts saving a value identical to what is stored (MySQL found-rows)', async () => {
    const { rows } = await rowsNow();
    const ch = emptyChanges();
    ch.updates.set(0, new Map<string, Cell>([['name', 'Renamed \\ again']]));
    const res = await apply(buildChanges(t.engine, t.schema, 'rowglow_e2e', cols, ['id'], rows, ch));
    expect(res).toMatchObject({ ok: true });
  });

  it('rolls everything back when a row changed underneath (guard)', async () => {
    const loaded = await rowsNow();
    // Someone else changes row 3's key after we loaded it.
    expect((await exec('UPDATE rowglow_e2e SET id = 99 WHERE id = 3', 'editor')).error).toBeUndefined();
    const ch = emptyChanges();
    ch.updates.set(0, new Map<string, Cell>([['name', 'should not stick']]));
    ch.updates.set(2, new Map<string, Cell>([['name', 'ghost']]));
    const res = await apply(buildChanges(t.engine, t.schema, 'rowglow_e2e', cols, ['id'], loaded.rows, ch));
    expect(res.ok).toBe(false);
    expect(res.failedIndex).toBe(1);
    expect(res.error?.code).toBe('row_not_found');
    const after = await rowsNow();
    expect(after.rows[0][after.columns.indexOf('name')]).toBe('Renamed \\ again'); // first update rolled back
  });

  it('refuses a statement that would touch more than one row', async () => {
    const res = await apply(["UPDATE rowglow_e2e SET note = 'mass'"]);
    expect(res.ok).toBe(false);
    expect(res.error?.code).toBe('row_count_mismatch');
    const after = await rowsNow();
    expect(after.rows.every((r) => r[after.columns.indexOf('note')] !== 'mass')).toBe(true);
  });

  it('deletes exactly the chosen row', async () => {
    const loaded = await rowsNow();
    const ch = emptyChanges();
    ch.deletes.add(1);
    const res = await apply(buildChanges(t.engine, t.schema, 'rowglow_e2e', cols, ['id'], loaded.rows, ch));
    expect(res).toMatchObject({ ok: true });
    const after = await rowsNow();
    expect(after.rows.map((r) => r[after.columns.indexOf('id')])).toEqual(['1', '99']);
  });

  it('blocks writes on a read-only connection', async () => {
    const saved = await rpc<{ id: string }>('connections.save', {
      connection: { name: `e2e ro ${t.engine}`, readOnly: true, ...t.connection },
    });
    cleanup.push(() => rpc('connections.delete', { id: saved.id }));
    const ro = await rpc<{ sessionId: string }>('session.open', { connectionId: saved.id });
    const run = async (sql: string) =>
      (await rpc<StatementOutcome[]>('query.execute', { sessionId: ro.sessionId, statements: [sql], limit: 10 }))[0];
    expect((await run('SELECT count(*) FROM rowglow_e2e')).error).toBeUndefined();
    expect((await run("UPDATE rowglow_e2e SET name = 'x' WHERE id = 1")).error?.code).toBe('read_only');
    expect((await run('DELETE FROM rowglow_e2e')).error?.code).toBe('read_only');
    if (t.engine === 'postgres') {
      expect((await run('SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE')).error?.code).toBe('read_only');
    }
    await expect(rpc('query.transaction', { sessionId: ro.sessionId, statements: ["UPDATE rowglow_e2e SET name = 'x' WHERE id = 1"] })).rejects.toThrow(/read-only/);
    await rpc('session.close', { sessionId: ro.sessionId });
    await rpc('connections.delete', { id: saved.id }); // stay within the free plan's 3 saved connections
    const check = await rowsNow();
    expect(check.rows[0][check.columns.indexOf('name')]).toBe('Renamed \\ again');
  });

  it('closes the session', async () => {
    await exec('DROP TABLE IF EXISTS rowglow_e2e', 'editor');
    await rpc('session.close', { sessionId });
    await rpc('connections.delete', { id: connectionId });
  });
});
