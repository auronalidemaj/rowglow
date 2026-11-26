/** Small fuzzy matcher: consecutive and word-start matches score higher. */
export function fuzzyScore(query: string, target: string): number {
  if (!query) return 1;
  const q = query.toLowerCase();
  const t = target.toLowerCase();
  if (t === q) return 1000;
  if (t.startsWith(q)) return 800 - t.length;
  const idx = t.indexOf(q);
  if (idx >= 0) return 600 - idx - t.length / 10;
  let score = 0;
  let ti = 0;
  let streak = 0;
  for (let qi = 0; qi < q.length; qi++) {
    const ch = q[qi];
    let found = false;
    while (ti < t.length) {
      if (t[ti] === ch) {
        const wordStart = ti === 0 || /[\s_.\-/]/.test(t[ti - 1]);
        streak++;
        score += 10 + streak * 5 + (wordStart ? 20 : 0);
        ti++;
        found = true;
        break;
      }
      streak = 0;
      ti++;
    }
    if (!found) return -1;
  }
  return score - t.length / 5;
}

export function fuzzyFilter<T>(items: T[], query: string, key: (t: T) => string, limit = 50): T[] {
  if (!query.trim()) return items.slice(0, limit);
  return items
    .map((item) => ({ item, s: fuzzyScore(query.trim(), key(item)) }))
    .filter((x) => x.s >= 0)
    .sort((a, b) => b.s - a.s)
    .slice(0, limit)
    .map((x) => x.item);
}
