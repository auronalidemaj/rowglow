import { describe, expect, it } from 'vitest';
import { ejsonCell, shellToJson } from './mongoShell';

const j = (s: string) => JSON.parse(shellToJson(s));

describe('shellToJson', () => {
  it('accepts mongosh syntax', () => {
    expect(j(`{ age: { $gt: 30 }, 'name': 'Zoë', ok: true, n: null, tags: ['a', "b",], }`)).toEqual({
      age: { $gt: 30 },
      name: 'Zoë',
      ok: true,
      n: null,
      tags: ['a', 'b'],
    });
  });
  it('converts shell helpers to Extended JSON', () => {
    expect(j(`{_id: ObjectId("65f0c0ffee0000000000abcd")}`)).toEqual({ _id: { $oid: '65f0c0ffee0000000000abcd' } });
    expect(j(`{at: ISODate("2024-05-01")}`)).toEqual({ at: { $date: '2024-05-01T00:00:00.000Z' } });
    expect(j(`{at: new Date("2024-05-01T10:00:00Z")}`)).toEqual({ at: { $date: '2024-05-01T10:00:00.000Z' } });
    expect(j(`{n: NumberLong(42), d: NumberDecimal("1.10"), i: NumberInt("7")}`)).toEqual({ n: { $numberLong: '42' }, d: { $numberDecimal: '1.10' }, i: 7 });
    expect(j(`{name: /^zo\\/e/i}`)).toEqual({ name: { $regularExpression: { pattern: '^zo\\/e', options: 'i' } } });
  });
  it('keeps big integers exact and handles comments', () => {
    expect(shellToJson('{ id: 9007199254740993 } // big')).toBe('{"id":{"$numberLong":"9007199254740993"}}');
    expect(shellToJson('/* all */')).toBe('{}');
    expect(j('[{ $match: {} }, { $limit: 5 }]')).toEqual([{ $match: {} }, { $limit: 5 }]);
  });
  it('reports errors with a line number', () => {
    expect(() => shellToJson('{ a: 1\n b: 2 }')).toThrow(/line 2/);
    expect(() => shellToJson('{ a: Foo(1) }')).toThrow(/Unknown function/);
  });
  it('renders cells', () => {
    expect(ejsonCell({ $oid: 'abc' }).text).toBe("ObjectId('abc')");
    expect(ejsonCell({ $date: '2024-05-01T10:00:00Z' }).text).toBe('2024-05-01 10:00:00 UTC');
    expect(ejsonCell({ a: 1, b: 2 }).text).toBe('{2}');
    expect(ejsonCell([1, 2, 3]).text).toBe('[3]');
  });
});
