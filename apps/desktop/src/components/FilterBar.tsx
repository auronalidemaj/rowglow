import { Code2, Plus, X } from 'lucide-react';
import { useEffect, useRef } from 'react';
import { FILTER_OPS, type Filter, type FilterOp } from '../lib/sqlgen';
import { uid } from '../lib/format';
import type { Column } from '../lib/types';
import { Button, IconButton } from './ui';

interface Props {
  columns: Column[];
  filters: Filter[];
  raw: boolean;
  rawWhere: string;
  onChange(filters: Filter[]): void;
  onRawChange(raw: boolean, where: string): void;
  onApply(): void;
  onClear(): void;
  focusKey: number;
}

export function newFilter(column = '*'): Filter {
  return { id: uid(), column, op: column === '*' ? 'contains' : '=', value: '', enabled: true };
}

export function FilterBar({ columns, filters, raw, rawWhere, onChange, onRawChange, onApply, onClear, focusKey }: Props) {
  const firstInput = useRef<HTMLInputElement | HTMLTextAreaElement | null>(null);
  useEffect(() => {
    firstInput.current?.focus();
  }, [focusKey, raw]);

  const update = (id: string, patch: Partial<Filter>) => onChange(filters.map((f) => (f.id === id ? { ...f, ...patch } : f)));
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      onApply();
    }
  };

  return (
    <div className="filter-bar">
      {raw ? (
        <div className="filter-row">
          <span className="filter-where">WHERE</span>
          <input
            ref={(el) => {
              firstInput.current = el;
            }}
            className="input mono filter-raw"
            value={rawWhere}
            placeholder="status = 'active' AND created_at > now() - interval '7 days'"
            spellCheck={false}
            onChange={(e) => onRawChange(true, e.target.value)}
            onKeyDown={onKey}
          />
        </div>
      ) : (
        filters.map((f, i) => {
          const noValue = f.op === 'is null' || f.op === 'is not null';
          return (
            <div className="filter-row" key={f.id}>
              <input
                type="checkbox"
                className="filter-check"
                checked={f.enabled}
                title={f.enabled ? 'Disable this filter' : 'Enable this filter'}
                onChange={(e) => update(f.id, { enabled: e.target.checked })}
              />
              <select className="select filter-col" value={f.column} onChange={(e) => update(f.id, { column: e.target.value, op: e.target.value === '*' ? 'contains' : f.op })}>
                <option value="*">Any column</option>
                {columns.map((c) => (
                  <option key={c.name} value={c.name}>
                    {c.name}
                  </option>
                ))}
              </select>
              <select
                className="select filter-op"
                value={f.op}
                onChange={(e) => update(f.id, { op: e.target.value as FilterOp })}
                disabled={f.column === '*'}
              >
                {(f.column === '*' ? (['contains', 'not contains'] as FilterOp[]) : FILTER_OPS).map((op) => (
                  <option key={op} value={op}>
                    {op}
                  </option>
                ))}
              </select>
              <input
                ref={
                  i === 0
                    ? (el) => {
                        firstInput.current = el;
                      }
                    : undefined
                }
                className="input mono filter-value"
                value={f.value}
                disabled={noValue}
                placeholder={noValue ? '' : f.op === 'in' ? 'a, b, c' : 'value'}
                spellCheck={false}
                onChange={(e) => update(f.id, { value: e.target.value })}
                onKeyDown={onKey}
              />
              <IconButton label="Remove filter" onClick={() => (filters.length === 1 ? onClear() : onChange(filters.filter((x) => x.id !== f.id)))}>
                <X size={14} />
              </IconButton>
              {i === filters.length - 1 && (
                <IconButton label="Add filter" onClick={() => onChange([...filters, newFilter(columns[0]?.name ?? '*')])}>
                  <Plus size={14} />
                </IconButton>
              )}
            </div>
          );
        })
      )}
      <div className="filter-actions">
        <IconButton label={raw ? 'Use filter builder' : 'Write a raw WHERE clause'} active={raw} onClick={() => onRawChange(!raw, rawWhere)}>
          <Code2 size={14} />
        </IconButton>
        <Button size="sm" variant="ghost" onClick={onClear}>
          Clear
        </Button>
        <Button size="sm" variant="primary" onClick={onApply}>
          Apply
        </Button>
      </div>
    </div>
  );
}
