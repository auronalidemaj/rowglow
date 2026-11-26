import clsx from 'clsx';
import { ArrowUpRight, Ban, Copy, KeyRound, PanelRightClose } from 'lucide-react';
import { useEffect, useState } from 'react';
import { isBinaryType, isComputedColumn, isJsonType, isNumericType } from '../lib/dialect';
import { tryPrettyJson } from '../lib/format';
import { copyText } from '../lib/platform';
import type { Cell } from '../lib/types';
import { useInspect } from '../state/inspect';
import { useStore } from '../state/store';
import { IconButton } from './ui';

function FieldEditor({
  value,
  type,
  editable,
  onCommit,
}: {
  value: Cell;
  type: string;
  editable: boolean;
  onCommit(v: Cell): void;
}) {
  const [draft, setDraft] = useState<string>(value ?? '');
  useEffect(() => setDraft(value ?? ''), [value]);
  const pretty = value !== null && (isJsonType(type) || value.startsWith('{') || value.startsWith('[')) ? tryPrettyJson(value) : null;
  const multiline = (value?.length ?? 0) > 60 || (value ?? '').includes('\n') || !!pretty;
  if (!editable || isBinaryType(type)) {
    return (
      <pre className={clsx('insp-value', value === null && 'is-null', isNumericType(type) && 'is-num')}>
        {value === null ? 'NULL' : (pretty ?? value)}
      </pre>
    );
  }
  const commit = () => {
    if (draft !== (value ?? '')) onCommit(draft);
  };
  return multiline ? (
    <textarea
      className={clsx('insp-input mono', value === null && 'is-null')}
      value={pretty && draft === value ? pretty : draft}
      placeholder={value === null ? 'NULL' : ''}
      rows={Math.min(14, Math.max(3, (pretty ?? draft).split('\n').length))}
      spellCheck={false}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
    />
  ) : (
    <input
      className={clsx('insp-input mono', value === null && 'is-null')}
      value={draft}
      placeholder={value === null ? 'NULL' : ''}
      spellCheck={false}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === 'Enter') (e.target as HTMLInputElement).blur();
        if (e.key === 'Escape') setDraft(value ?? '');
      }}
    />
  );
}

export function Inspector({ width }: { width: number }) {
  const target = useInspect((s) => s.target);
  const toggle = useStore((s) => s.toggleInspector);
  const toast = useStore((s) => s.toast);
  const [filter, setFilter] = useState('');

  return (
    <aside className="inspector" style={{ width }}>
      <div className="inspector-head">
        <span className="inspector-title">{target?.title ?? 'Inspector'}</span>
        <IconButton label="Hide inspector" onClick={toggle}>
          <PanelRightClose size={14} />
        </IconButton>
      </div>
      {!target ? (
        <div className="inspector-empty">Select a row to see every field here — long text, JSON and foreign keys included.</div>
      ) : (
        <>
          <div className="inspector-search">
            <input className="input input-sm" placeholder="Filter fields" value={filter} onChange={(e) => setFilter(e.target.value)} />
          </div>
          <div className="inspector-body">
            {target.columns.map((col, i) => {
              if (filter && !col.name.toLowerCase().includes(filter.toLowerCase())) return null;
              const def = target.colDefs?.find((d) => d.name === col.name);
              const fk = target.fks?.get(col.name);
              const value = target.values[i];
              return (
                <div className="insp-field" key={col.name}>
                  <div className="insp-label">
                    {def?.isPrimaryKey && <KeyRound size={10} className="pk-icon" />}
                    <span className="insp-name">{col.name}</span>
                    <span className="insp-type">{def?.dataType || col.typeName}</span>
                    <span className="insp-actions">
                      {fk && value !== null && target.onFollow && (
                        <button className="mini-btn" title={`Open ${fk.refTable}`} onClick={() => target.onFollow?.(fk)}>
                          <ArrowUpRight size={11} /> {fk.refTable}
                        </button>
                      )}
                      {target.editable && !isComputedColumn(def?.extra) && def?.nullable !== false && value !== null && (
                        <button className="mini-btn" title="Set NULL" onClick={() => target.onChange?.(i, null)}>
                          <Ban size={11} />
                        </button>
                      )}
                      <button className="mini-btn" title="Copy value" onClick={() => copyText(value ?? 'NULL').then(() => toast('success', 'Copied'))}>
                        <Copy size={11} />
                      </button>
                    </span>
                  </div>
                  <FieldEditor
                    value={value}
                    type={def?.dataType || col.typeName}
                    editable={target.editable && !isComputedColumn(def?.extra)}
                    onCommit={(v) => target.onChange?.(i, v)}
                  />
                </div>
              );
            })}
          </div>
        </>
      )}
    </aside>
  );
}
