import { describe, expect, it } from 'vitest';
import { splitStatements, statementAt, isWriteStatement, dangerousReason } from './sqlsplit';
import { buildChanges, buildWhere, emptyChanges, buildSelect, buildOrderBy, NoKeyError } from './sqlgen';
import { quoteLiteral, sqlValue, quoteIdent, isComputedColumn } from './dialect';
import { parseConnectionUrl } from './connurl';

const texts = (sql: string, e: 'postgres' | 'mysql' | 'sqlite' | 'mssql' | 'duckdb' = 'postgres') => splitStatements(sql, e).map((s) => s.text);

describe('splitStatements', () => {
  it('splits simple statements', () => {
    expect(texts('select 1; select 2;\nselect 3')).toEqual(['select 1', 'select 2', 'select 3']);
  });
  it('ignores semicolons in strings, identifiers and comments', () => {
    expect(texts(`select ';' as "a;b"; -- c;d\nselect /* ; */ 2`)).toEqual([`select ';' as "a;b"`, 'select /* ; */ 2']);
  });
  it('handles doubled quotes', () => {
    expect(texts(`select 'it''s;fine'; select 2`)).toEqual([`select 'it''s;fine'`, 'select 2']);
  });
  it('handles postgres dollar quoting', () => {
    const sql = `create function f() returns int as $$ begin return 1; end; $$ language plpgsql; select f();`;
    expect(texts(sql)).toEqual([
      'create function f() returns int as $$ begin return 1; end; $$ language plpgsql',
      'select f()',
    ]);
    expect(texts(`do $body$ begin perform 1; end $body$; select 1`)).toHaveLength(2);
  });
  it('handles postgres E strings and nested comments', () => {
    expect(texts(`select E'a\\';b'; /* x /* y; */ ; */ select 2`)).toEqual([`select E'a\\';b'`, 'select 2']);
  });
  it('handles mysql backslash escapes, # comments and backticks', () => {
    expect(texts("select 'a\\';b' ; # hi;\nselect `x;y` from t", 'mysql')).toEqual(["select 'a\\';b'", 'select `x;y` from t']);
  });
  it('handles mysql DELIMITER', () => {
    const sql = `DELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END$$\nDELIMITER ;\nCALL p();`;
    expect(texts(sql, 'mysql')).toEqual(['CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END', 'CALL p()']);
  });
  it('keeps mysql procedure bodies together without DELIMITER', () => {
    const sql = `CREATE PROCEDURE p() BEGIN IF 1 THEN SELECT 1; END IF; SELECT 2; END; CALL p();`;
    expect(texts(sql, 'mysql')).toEqual(['CREATE PROCEDURE p() BEGIN IF 1 THEN SELECT 1; END IF; SELECT 2; END', 'CALL p()']);
  });
  it('keeps sqlite trigger bodies together', () => {
    const sql = `CREATE TRIGGER t AFTER INSERT ON a BEGIN UPDATE b SET x = CASE WHEN 1 THEN 2 ELSE 3 END; DELETE FROM c; END; select 1;`;
    expect(texts(sql, 'sqlite')).toEqual([
      'CREATE TRIGGER t AFTER INSERT ON a BEGIN UPDATE b SET x = CASE WHEN 1 THEN 2 ELSE 3 END; DELETE FROM c; END',
      'select 1',
    ]);
  });
  it('still splits plain BEGIN/COMMIT transactions', () => {
    expect(texts('BEGIN; update t set a = 1; COMMIT;', 'sqlite')).toEqual(['BEGIN', 'update t set a = 1', 'COMMIT']);
  });
  it('finds the statement at the cursor', () => {
    const sql = 'select 1;\n\nselect 2;';
    const stmts = splitStatements(sql, 'postgres');
    expect(statementAt(stmts, 3, sql)?.text).toBe('select 1');
    expect(statementAt(stmts, 9, sql)?.text).toBe('select 1');
    expect(statementAt(stmts, sql.length, sql)?.text).toBe('select 2');
  });
  it('never runs the statement above from a blank line', () => {
    const sql = 'delete from t where id = 1;\n\n\nselect 2;';
    const stmts = splitStatements(sql, 'postgres');
    expect(statementAt(stmts, 28, sql)).toBeNull(); // empty line between them
    expect(statementAt(stmts, 27, sql)?.text).toBe('delete from t where id = 1'); // right after ;
    const trailing = 'select 1;   ';
    expect(statementAt(splitStatements(trailing, 'postgres'), trailing.length, trailing)?.text).toBe('select 1');
    const below = 'update t set a = 1 where id = 2;\n';
    expect(statementAt(splitStatements(below, 'postgres'), below.length, below)).toBeNull();
  });
});

describe('isWriteStatement', () => {
  it('classifies', () => {
    expect(isWriteStatement('select * from t')).toBe(false);
    expect(isWriteStatement('  -- x\n SELECT 1')).toBe(false);
    expect(isWriteStatement('with x as (select 1) select * from x')).toBe(false);
    expect(isWriteStatement('with x as (delete from t returning *) select * from x')).toBe(true);
    expect(isWriteStatement('update t set a = 1')).toBe(true);
    expect(isWriteStatement('drop table t')).toBe(true);
    expect(isWriteStatement("select 'update' from t")).toBe(false);
    expect(isWriteStatement('select * into new_t from t')).toBe(true);
    expect(isWriteStatement('explain select 1')).toBe(false);
  });
});

describe('dangerousReason', () => {
  it('flags mass changes and drops', () => {
    expect(dangerousReason('delete from users')).toMatch(/every row in users/);
    expect(dangerousReason('DELETE FROM users WHERE id = 1')).toBeNull();
    expect(dangerousReason('update "shop"."orders" set total = 0')).toMatch(/every row/);
    expect(dangerousReason("update t set note = 'where' ")).toMatch(/every row/);
    expect(dangerousReason('update t set a = 1 -- where id = 1')).toMatch(/every row/);
    expect(dangerousReason('update t set a = 1 where id = 1')).toBeNull();
    expect(dangerousReason('truncate table t')).toMatch(/TRUNCATE/);
    expect(dangerousReason('drop table t')).toMatch(/DROP TABLE/);
    expect(dangerousReason('alter table t drop column x')).toMatch(/ALTER/);
    expect(dangerousReason('alter table t add column x int')).toBeNull();
    expect(dangerousReason('select * from t')).toBeNull();
    expect(dangerousReason('with x as (select 1) select * from x')).toBeNull();
  });
});

describe('dialect', () => {
  it('quotes literals', () => {
    expect(quoteLiteral('postgres', "O'Brien\\")).toBe("'O''Brien\\'");
    expect(quoteLiteral('mysql', "O'Brien\\")).toBe("'O''Brien\\\\'");
  });
  it('types values', () => {
    expect(sqlValue('postgres', '42', 'int4')).toBe('42');
    expect(sqlValue('postgres', '42abc', 'int4')).toBe("'42abc'");
    expect(sqlValue('postgres', null, 'text')).toBe('NULL');
    expect(sqlValue('postgres', 'true', 'bool')).toBe('TRUE');
    expect(sqlValue('postgres', '12', 'text')).toBe("'12'");
    expect(sqlValue('mysql', '0xA1B2', 'binary(16)')).toBe("X'A1B2'");
    expect(sqlValue('mysql', '0xA1B2', 'varchar(10)')).toBe("'0xA1B2'");
    expect(sqlValue('sqlite', "x'0102'", 'blob')).toBe("X'0102'");
    expect(sqlValue('sqlite', "x'0102'", 'text')).toBe("'x''0102'''");
    expect(sqlValue('mysql', 'a\\b', 'text')).toBe("'a\\\\b'");
  });
});

describe('sqlgen', () => {
  const cols = [
    { name: 'id', type: 'int4' },
    { name: 'name', type: 'text' },
    { name: 'note', type: 'text' },
  ];
  it('builds filters', () => {
    expect(
      buildWhere(
        'postgres',
        [
          { id: '1', column: 'name', op: 'contains', value: '50%', enabled: true },
          { id: '2', column: 'id', op: '>', value: '3', enabled: true },
          { id: '3', column: 'note', op: 'is null', value: '', enabled: true },
          { id: '4', column: 'id', op: '=', value: '9', enabled: false },
        ],
        cols,
      ),
    ).toBe(`WHERE CAST("name" AS text) ILIKE '%50\\%%' ESCAPE '\\' AND "id" > 3 AND "note" IS NULL`);
    expect(buildWhere('mysql', [{ id: '1', column: 'id', op: 'in', value: '1, 2,3', enabled: true }], cols)).toBe(
      'WHERE `id` IN (1, 2, 3)',
    );
    expect(buildWhere('postgres', [], cols, ' where id = 1 ')).toBe('WHERE id = 1');
  });
  it('builds selects', () => {
    expect(
      buildSelect('postgres', 'public', 'users', {
        where: 'WHERE "id" > 3',
        orderBy: buildOrderBy('postgres', [], ['id']),
        limit: 100,
        offset: 200,
      }),
    ).toBe('SELECT * FROM "public"."users" WHERE "id" > 3 ORDER BY "id" LIMIT 100 OFFSET 200');
  });
  it('builds staged changes', () => {
    const rows = [
      ['1', 'Ann', null],
      ['2', 'Bob', 'x'],
    ];
    const ch = emptyChanges();
    ch.updates.set(0, new Map([['name', "Ann O'Neil"], ['note', null]]));
    ch.deletes.add(1);
    ch.inserts.push({ key: 'n1', values: new Map([['name', 'Cy']]) });
    ch.inserts.push({ key: 'n2', values: new Map() });
    expect(buildChanges('postgres', 'public', 'users', cols, ['id'], rows, ch)).toEqual([
      `UPDATE "public"."users" SET "name" = 'Ann O''Neil', "note" = NULL WHERE "id" = 1`,
      `DELETE FROM "public"."users" WHERE "id" = 2`,
      `INSERT INTO "public"."users" ("name") VALUES ('Cy')`,
      `INSERT INTO "public"."users" DEFAULT VALUES`,
    ]);
    expect(() => buildChanges('postgres', 'public', 'users', cols, [], rows, ch)).toThrow(NoKeyError);
  });
});

describe('SQL Server and DuckDB', () => {
  it('splits T-SQL batches on GO and keeps [bracketed;names]', () => {
    expect(texts('SELECT 1 AS [a;b]\nGO\nCREATE PROCEDURE p AS\nBEGIN\n  SELECT 1;\n  BEGIN TRAN;\n  UPDATE t SET a = 1;\n  COMMIT;\nEND\ngo\nSELECT 2', 'mssql')).toEqual([
      'SELECT 1 AS [a;b]',
      'CREATE PROCEDURE p AS\nBEGIN\n  SELECT 1;\n  BEGIN TRAN;\n  UPDATE t SET a = 1;\n  COMMIT;\nEND',
      'SELECT 2',
    ]);
    // GO inside a word or a string is not a separator
    expect(texts("SELECT 'GO' AS x; SELECT 1 AS going", 'mssql')).toEqual(["SELECT 'GO' AS x", 'SELECT 1 AS going']);
  });
  it('quotes and pages for SQL Server', () => {
    expect(quoteIdent('mssql', 'a]b')).toBe('[a]]b]');
    expect(quoteLiteral('mssql', "Zoë's")).toBe("N'Zoë''s'");
    expect(sqlValue('mssql', '0x0102FF', 'varbinary')).toBe('0x0102FF');
    expect(sqlValue('mssql', '1', 'bit')).toBe('1');
    expect(isComputedColumn('identity(1,1)')).toBe(true);
    expect(buildSelect('mssql', 'dbo', 'users', { where: '', orderBy: '', limit: 50, offset: 100 })).toBe(
      'SELECT * FROM [dbo].[users] ORDER BY (SELECT NULL) OFFSET 100 ROWS FETCH NEXT 50 ROWS ONLY',
    );
    expect(buildSelect('mssql', 'dbo', 'users', { where: 'WHERE [id] > 1', orderBy: 'ORDER BY [id]', limit: 10 })).toBe(
      'SELECT * FROM [dbo].[users] WHERE [id] > 1 ORDER BY [id] OFFSET 0 ROWS FETCH NEXT 10 ROWS ONLY',
    );
  });
  it('treats any write in a T-SQL batch as a write', () => {
    expect(isWriteStatement('SELECT 1\nDELETE FROM t', 'mssql')).toBe(true);
    expect(isWriteStatement('SELECT * FROM [delete] WHERE note = \'drop\'', 'mssql')).toBe(false);
    expect(dangerousReason('SELECT 1\nDELETE FROM users', 'mssql')).toMatch(/every row/);
  });
  it('sends DuckDB blobs back with from_hex', () => {
    expect(sqlValue('duckdb', '0xCAFE', 'blob')).toBe("from_hex('CAFE')");
    expect(texts('select $$a;b$$; select 2', 'duckdb')).toEqual(['select $$a;b$$', 'select 2']);
  });
  it('parses new connection URLs', () => {
    expect(parseConnectionUrl('sqlserver://sa:pw@db.local:1444/shop')?.patch).toMatchObject({ engine: 'mssql', host: 'db.local', port: 1444, username: 'sa', database: 'shop' });
    expect(parseConnectionUrl('rediss://:secret@cache:6380/2')).toMatchObject({ patch: { engine: 'redis', port: 6380, database: '2', sslMode: 'require' }, password: 'secret' });
    expect(parseConnectionUrl('mongodb+srv://me:pw@cluster0.abc.mongodb.net/app?retryWrites=true')).toMatchObject({
      patch: { engine: 'mongodb', username: 'me', database: 'app' },
      password: 'pw',
    });
    expect(parseConnectionUrl('/Users/me/data/analytics.duckdb')?.patch).toEqual({ engine: 'duckdb', filePath: '/Users/me/data/analytics.duckdb' });
  });
});
