/**
 * Converts mongo-shell style input into strict Extended JSON text, so users
 * can type what they'd type in mongosh:
 *
 *   { age: { $gt: 30 }, _id: ObjectId("65f0…"), name: /^zo/i, at: ISODate("2024-01-01") }
 *
 * Unquoted keys, 'single quotes', trailing commas, comments, regex literals
 * and the shell helpers (ObjectId, ISODate, new Date, NumberLong, NumberInt,
 * NumberDecimal, UUID, Timestamp, BinData, MinKey, MaxKey) are supported.
 * Numbers are copied verbatim, so large integers keep every digit.
 */
export class ShellSyntaxError extends Error {
  constructor(
    message: string,
    public position: number,
  ) {
    super(message);
  }
}

export function shellToJson(input: string): string {
  let i = 0;
  const s = input;
  const fail = (msg: string): never => {
    const line = s.slice(0, i).split('\n').length;
    throw new ShellSyntaxError(`${msg} (line ${line})`, i);
  };

  const ws = () => {
    for (;;) {
      while (i < s.length && /\s/.test(s[i])) i++;
      if (s.startsWith('//', i)) {
        while (i < s.length && s[i] !== '\n') i++;
      } else if (s.startsWith('/*', i)) {
        const end = s.indexOf('*/', i + 2);
        i = end < 0 ? s.length : end + 2;
      } else return;
    }
  };

  const ident = (): string => {
    const m = /^[A-Za-z_$][\w$]*/.exec(s.slice(i));
    if (!m) fail('Expected a name');
    i += m![0].length;
    return m![0];
  };

  const str = (): string => {
    const q = s[i];
    i++;
    let out = '';
    while (i < s.length && s[i] !== q) {
      if (s[i] === '\\') {
        const n = s[i + 1];
        if (n === undefined) fail('Unfinished escape');
        if (n === "'" ) out += "'";
        else out += JSON.parse(`"\\${n === 'u' ? s.slice(i + 1, i + 6) : n}"`);
        i += n === 'u' ? 6 : 2;
        continue;
      }
      if (s[i] === '\n') fail('Unterminated string');
      out += s[i];
      i++;
    }
    if (s[i] !== q) fail('Unterminated string');
    i++;
    return out;
  };

  const num = (): string => {
    const m = /^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?/.exec(s.slice(i));
    if (!m) fail('Expected a number');
    i += m![0].length;
    let t = m![0].replace(/^\+/, '');
    if (t.startsWith('.')) t = '0' + t;
    if (t.startsWith('-.')) t = '-0' + t.slice(1);
    if (/\.$/.test(t)) t += '0';
    // Integers beyond what a double holds exactly become NumberLong.
    if (/^-?\d+$/.test(t) && !Number.isSafeInteger(Number(t))) return JSON.stringify({ $numberLong: t });
    return t;
  };

  const args = (): string[] => {
    ws();
    if (s[i] !== '(') fail('Expected (');
    i++;
    const out: string[] = [];
    ws();
    while (s[i] !== ')') {
      out.push(value());
      ws();
      if (s[i] === ',') {
        i++;
        ws();
      } else if (s[i] !== ')') fail('Expected , or )');
    }
    i++;
    return out;
  };
  const strArg = (a: string[], idx = 0): string => {
    const v = a[idx];
    if (v === undefined) return '';
    try {
      const p = JSON.parse(v);
      return typeof p === 'string' ? p : String(p);
    } catch {
      return v;
    }
  };

  const call = (name: string): string => {
    switch (name) {
      case 'ObjectId':
      case 'ObjectID': {
        const a = args();
        return a.length ? JSON.stringify({ $oid: strArg(a) }) : fail('ObjectId needs a hex string');
      }
      case 'ISODate':
      case 'Date': {
        const a = args();
        if (!a.length) return JSON.stringify({ $date: new Date().toISOString() });
        const raw = strArg(a);
        const d = /^-?\d+$/.test(raw) ? new Date(Number(raw)) : new Date(/^\d{4}-\d{2}-\d{2}$/.test(raw) ? raw + 'T00:00:00Z' : raw);
        if (Number.isNaN(d.getTime())) fail(`Invalid date ${raw}`);
        return JSON.stringify({ $date: d.toISOString() });
      }
      case 'NumberLong':
        return JSON.stringify({ $numberLong: strArg(args()) });
      case 'NumberInt':
        return String(parseInt(strArg(args()), 10));
      case 'NumberDecimal':
        return JSON.stringify({ $numberDecimal: strArg(args()) });
      case 'UUID':
        return JSON.stringify({ $uuid: strArg(args()) });
      case 'Timestamp': {
        const a = args();
        return `{"$timestamp":{"t":${Number(strArg(a, 0)) || 0},"i":${Number(strArg(a, 1)) || 0}}}`;
      }
      case 'BinData': {
        const a = args();
        const sub = Number(strArg(a, 0)) || 0;
        return JSON.stringify({ $binary: { base64: strArg(a, 1), subType: sub.toString(16).padStart(2, '0') } });
      }
      case 'MinKey':
        if (s[i] === '(' || /^\s*\(/.test(s.slice(i))) args();
        return '{"$minKey":1}';
      case 'MaxKey':
        if (s[i] === '(' || /^\s*\(/.test(s.slice(i))) args();
        return '{"$maxKey":1}';
      default:
        return fail(`Unknown function ${name}()`);
    }
  };

  const regex = (): string => {
    i++; // opening /
    let pat = '';
    let inClass = false;
    while (i < s.length && (s[i] !== '/' || inClass)) {
      if (s[i] === '\\') {
        pat += s[i] + (s[i + 1] ?? '');
        i += 2;
        continue;
      }
      if (s[i] === '[') inClass = true;
      if (s[i] === ']') inClass = false;
      if (s[i] === '\n') fail('Unterminated regex');
      pat += s[i];
      i++;
    }
    if (s[i] !== '/') fail('Unterminated regex');
    i++;
    const flags = /^[a-z]*/.exec(s.slice(i))![0];
    i += flags.length;
    return JSON.stringify({ $regularExpression: { pattern: pat, options: flags.split('').sort().join('') } });
  };

  const value = (): string => {
    ws();
    const c = s[i];
    if (c === undefined) return fail('Unexpected end of input');
    if (c === '{') return object();
    if (c === '[') return array();
    if (c === '"' || c === "'") return JSON.stringify(str());
    if (c === '/') return regex();
    if (/[\d.+-]/.test(c)) {
      if (s.startsWith('-Infinity', i)) {
        i += 9;
        return '{"$numberDouble":"-Infinity"}';
      }
      return num();
    }
    const word = ident();
    switch (word) {
      case 'true':
      case 'false':
      case 'null':
        return word;
      case 'undefined':
        return 'null';
      case 'Infinity':
        return '{"$numberDouble":"Infinity"}';
      case 'NaN':
        return '{"$numberDouble":"NaN"}';
      case 'new':
        ws();
        return call(ident());
      default:
        return call(word);
    }
  };

  const object = (): string => {
    i++; // {
    const parts: string[] = [];
    for (;;) {
      ws();
      if (s[i] === '}') {
        i++;
        break;
      }
      let key: string;
      if (s[i] === '"' || s[i] === "'") key = str();
      else if (/[\d]/.test(s[i] ?? '')) key = num();
      else key = ident();
      ws();
      if (s[i] !== ':') fail(`Expected : after ${key}`);
      i++;
      parts.push(`${JSON.stringify(key)}:${value()}`);
      ws();
      if (s[i] === ',') i++;
      else if (s[i] !== '}') fail('Expected , or }');
    }
    return `{${parts.join(',')}}`;
  };

  const array = (): string => {
    i++; // [
    const parts: string[] = [];
    for (;;) {
      ws();
      if (s[i] === ']') {
        i++;
        break;
      }
      parts.push(value());
      ws();
      if (s[i] === ',') i++;
      else if (s[i] !== ']') fail('Expected , or ]');
    }
    return `[${parts.join(',')}]`;
  };

  ws();
  if (i >= s.length) return '{}';
  const out = value();
  ws();
  if (s[i] === ';') i++;
  ws();
  if (i < s.length) fail('Unexpected text after the value');
  return out;
}

/** Short, readable form of an Extended JSON value for table cells. */
export function ejsonCell(v: unknown): { text: string; kind: 'null' | 'num' | 'str' | 'bool' | 'id' | 'date' | 'obj' | 'arr' } {
  if (v === null || v === undefined) return { text: 'null', kind: 'null' };
  if (typeof v === 'number') return { text: String(v), kind: 'num' };
  if (typeof v === 'boolean') return { text: String(v), kind: 'bool' };
  if (typeof v === 'string') return { text: v, kind: 'str' };
  if (Array.isArray(v)) return { text: `[${v.length}]`, kind: 'arr' };
  const o = v as Record<string, unknown>;
  const keys = Object.keys(o);
  if (keys.length === 1) {
    const k = keys[0];
    const x = o[k] as Record<string, unknown> | string;
    if (k === '$oid') return { text: `ObjectId('${x}')`, kind: 'id' };
    if (k === '$date') {
      if (typeof x === 'string') return { text: x.replace('T', ' ').replace(/\.000Z$|Z$/, ' UTC'), kind: 'date' };
      const ms = Number((x as { $numberLong?: string }).$numberLong);
      return { text: Number.isFinite(ms) ? new Date(ms).toISOString() : JSON.stringify(x), kind: 'date' };
    }
    if (k === '$numberDecimal' || k === '$numberLong' || k === '$numberDouble' || k === '$numberInt') return { text: String(x), kind: 'num' };
    if (k === '$binary') return { text: `Binary(${(x as { subType?: string }).subType ?? '00'})`, kind: 'obj' };
    if (k === '$uuid') return { text: `UUID('${x}')`, kind: 'id' };
    if (k === '$regularExpression') {
      const r = x as { pattern: string; options: string };
      return { text: `/${r.pattern}/${r.options}`, kind: 'str' };
    }
    if (k === '$timestamp') return { text: `Timestamp(${JSON.stringify(x)})`, kind: 'obj' };
  }
  return { text: `{${keys.length}}`, kind: 'obj' };
}
