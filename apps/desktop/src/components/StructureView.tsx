import { ArrowUpRight, KeyRound, Link2 } from 'lucide-react';
import type { ReactNode } from 'react';
import type { TableStructure } from '../lib/types';
import { useStore, type Workspace } from '../state/store';
import { Spinner } from './ui';

function Section({ title, count, children }: { title: string; count: number; children: ReactNode }) {
  return (
    <section className="struct-section">
      <h3 className="struct-title">
        {title} <span className="struct-count">{count}</span>
      </h3>
      {count === 0 ? <div className="struct-empty">None</div> : children}
    </section>
  );
}

export function StructureView({ ws, schema, table, structure }: { ws: Workspace; schema: string; table: string; structure: TableStructure | null }) {
  const openTable = useStore((s) => s.openTable);
  if (!structure) {
    return (
      <div className="center-fill">
        <Spinner />
      </div>
    );
  }
  const fkByCol = new Map(structure.foreignKeys.filter((f) => f.columns.length === 1).map((f) => [f.columns[0], f]));
  return (
    <div className="structure-view">
      <Section title="Columns" count={structure.columns.length}>
        <table className="struct-table">
          <thead>
            <tr>
              <th style={{ width: 28 }}>#</th>
              <th>Name</th>
              <th>Type</th>
              <th>Nullable</th>
              <th>Default</th>
              <th>Extra</th>
              <th>Comment</th>
            </tr>
          </thead>
          <tbody>
            {structure.columns.map((c, i) => {
              const fk = fkByCol.get(c.name);
              return (
                <tr key={c.name}>
                  <td className="dim">{i + 1}</td>
                  <td className="mono strong">
                    {c.isPrimaryKey && <KeyRound size={11} className="pk-icon" />} {c.name}
                    {fk && (
                      <button
                        className="fk-chip"
                        title={`References ${fk.refTable}(${fk.refColumns.join(', ')})`}
                        onClick={() => openTable(ws.sessionId, fk.refSchema, { name: fk.refTable, kind: 'table' }, { view: 'structure' })}
                      >
                        <Link2 size={10} /> {fk.refTable}
                      </button>
                    )}
                  </td>
                  <td className="mono type">{c.dataType}</td>
                  <td>{c.nullable ? <span className="dim">yes</span> : <span className="pill">NOT NULL</span>}</td>
                  <td className="mono dim">{c.defaultValue ?? ''}</td>
                  <td className="dim">{c.extra ?? ''}</td>
                  <td className="dim">{c.comment ?? ''}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </Section>
      <Section title="Indexes" count={structure.indexes.length}>
        <table className="struct-table">
          <thead>
            <tr>
              <th>Name</th>
              <th>Columns</th>
              <th>Unique</th>
              <th>Definition</th>
            </tr>
          </thead>
          <tbody>
            {structure.indexes.map((ix) => (
              <tr key={ix.name}>
                <td className="mono strong">
                  {ix.primary && <KeyRound size={11} className="pk-icon" />} {ix.name}
                </td>
                <td className="mono">{ix.columns.join(', ')}</td>
                <td>{ix.unique ? <span className="pill">UNIQUE</span> : ''}</td>
                <td className="mono dim wrap">{ix.definition ?? ''}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </Section>
      <Section title="Foreign keys" count={structure.foreignKeys.length}>
        <table className="struct-table">
          <thead>
            <tr>
              <th>Name</th>
              <th>Columns</th>
              <th>References</th>
              <th>On update</th>
              <th>On delete</th>
            </tr>
          </thead>
          <tbody>
            {structure.foreignKeys.map((fk) => (
              <tr key={fk.name}>
                <td className="mono strong">{fk.name}</td>
                <td className="mono">{fk.columns.join(', ')}</td>
                <td className="mono">
                  <button className="link-btn" onClick={() => openTable(ws.sessionId, fk.refSchema, { name: fk.refTable, kind: 'table' })}>
                    {fk.refSchema !== schema ? `${fk.refSchema}.` : ''}
                    {fk.refTable}({fk.refColumns.join(', ')}) <ArrowUpRight size={11} />
                  </button>
                </td>
                <td className="dim">{fk.onUpdate}</td>
                <td className="dim">{fk.onDelete}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </Section>
      <Section title="Referenced by" count={structure.referencedBy.length}>
        <table className="struct-table">
          <thead>
            <tr>
              <th>Table</th>
              <th>Their columns</th>
              <th>Our columns</th>
              <th>On delete</th>
            </tr>
          </thead>
          <tbody>
            {structure.referencedBy.map((fk) => (
              <tr key={fk.name}>
                <td className="mono">
                  <button className="link-btn" onClick={() => openTable(ws.sessionId, fk.refSchema, { name: fk.refTable, kind: 'table' })}>
                    {fk.refSchema !== schema ? `${fk.refSchema}.` : ''}
                    {fk.refTable} <ArrowUpRight size={11} />
                  </button>
                </td>
                <td className="mono">{fk.refColumns.join(', ')}</td>
                <td className="mono">{fk.columns.join(', ')}</td>
                <td className="dim">{fk.onDelete}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </Section>
      <Section title="Constraints" count={structure.constraints.length}>
        <table className="struct-table">
          <thead>
            <tr>
              <th>Name</th>
              <th>Type</th>
              <th>Definition</th>
            </tr>
          </thead>
          <tbody>
            {structure.constraints.map((c) => (
              <tr key={c.name}>
                <td className="mono strong">{c.name}</td>
                <td>
                  <span className="pill">{c.kind}</span>
                </td>
                <td className="mono dim wrap">{c.definition}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </Section>
      <Section title="Triggers" count={structure.triggers.length}>
        <table className="struct-table">
          <thead>
            <tr>
              <th>Name</th>
              <th>Definition</th>
            </tr>
          </thead>
          <tbody>
            {structure.triggers.map((t) => (
              <tr key={t.name}>
                <td className="mono strong">{t.name}</td>
                <td className="mono dim wrap">{t.definition}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </Section>
      <div className="struct-footer dim">
        {table} · {structure.columns.length} columns
      </div>
    </div>
  );
}
