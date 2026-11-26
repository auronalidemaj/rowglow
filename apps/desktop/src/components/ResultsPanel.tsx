import clsx from 'clsx';
import { AlertTriangle, CheckCircle2, Copy, Download, MessageSquareText, Sparkles, TerminalSquare } from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';
import { toDelimited, toInserts, toJSON, toMarkdown } from '../lib/exporters';
import { formatCount, formatDuration, plural } from '../lib/format';
import { copyText, pickSaveFile } from '../lib/platform';
import { call, errorMessage } from '../lib/rpc';
import type { Engine, StatementOutcome } from '../lib/types';
import { useInspect } from '../state/inspect';
import { useStore } from '../state/store';
import { DataGrid } from './DataGrid';
import { EmptyState, IconButton, Kbd, Spinner, useMenu } from './ui';
import { mod } from '../lib/platform';

function label(o: StatementOutcome, i: number): string {
  if (o.error) return `Error`;
  const kw = (/^[\s(]*([A-Za-z]+)/.exec(o.sql.replace(/^\s*(--[^\n]*\n|\/\*[\s\S]*?\*\/)*/g, ''))?.[1] ?? '').toUpperCase();
  if (o.result?.hasRows) return `${kw === 'SELECT' || kw === 'WITH' || !kw ? `Result ${i + 1}` : kw}`;
  return kw || `Statement ${i + 1}`;
}

export function ResultsPanel({
  outcomes,
  running,
  elapsed,
  engine,
  active,
  tabId,
  onShowError,
  onFixWithAi,
}: {
  outcomes: StatementOutcome[] | null;
  running: boolean;
  elapsed: number;
  engine: Engine;
  active: boolean;
  tabId: string;
  onShowError?(o: StatementOutcome): void;
  /** Ask the AI assistant to fix the failed statement. */
  onFixWithAi?(o: StatementOutcome): void;
}) {
  const [sel, setSel] = useState(0);
  const [activeRow, setActiveRow] = useState<number | null>(null);
  const [menu, openMenu] = useMenu();
  const toast = useStore((s) => s.toast);
  const setInspect = useInspect((s) => s.set);
  const clearInspect = useInspect((s) => s.clear);

  useEffect(() => {
    if (!outcomes) return;
    const err = outcomes.findIndex((o) => o.error);
    if (err >= 0) setSel(err);
    else {
      let last = -1;
      outcomes.forEach((o, i) => o.result?.hasRows && (last = i));
      setSel(last >= 0 ? last : outcomes.length - 1);
    }
    setActiveRow(null);
  }, [outcomes]);

  const current = outcomes?.[sel];
  const result = current?.result;

  useEffect(() => {
    if (!active || !result?.hasRows || activeRow === null || !result.rows[activeRow]) {
      clearInspect(tabId);
      return;
    }
    setInspect({
      owner: tabId,
      title: `Result · row ${activeRow + 1}`,
      columns: result.columns,
      values: result.rows[activeRow],
      editable: false,
    });
  }, [active, activeRow, result, tabId, setInspect, clearInspect]);
  useEffect(() => () => clearInspect(tabId), [clearInspect, tabId]);

  const totals = useMemo(() => {
    if (!outcomes) return null;
    const ms = outcomes.reduce((a, o) => a + (o.result?.durationMs ?? 0), 0);
    return {
      ms,
      n: outcomes.length,
      errors: outcomes.filter((o) => o.error).length,
    };
  }, [outcomes]);

  if (!outcomes && !running) {
    return (
      <div className="results-empty">
        <EmptyState icon={<TerminalSquare size={22} />} title="Run a query to see results">
          <div className="shortcut-hints">
            <span>
              <Kbd>{mod}↵</Kbd> run statement at cursor
            </span>
            <span>
              <Kbd>⇧{mod}↵</Kbd> run all
            </span>
            <span>
              <Kbd>⇧{mod}F</Kbd> format
            </span>
            <span>
              <Kbd>{mod}S</Kbd> save query
            </span>
          </div>
        </EmptyState>
      </div>
    );
  }

  const exportResult = async (format: 'csv' | 'json' | 'sql' | 'md') => {
    if (!result) return;
    const ext = format === 'md' ? 'md' : format;
    const path = await pickSaveFile('Export result', `result.${ext}`, [{ name: format.toUpperCase(), extensions: [ext] }]);
    if (!path) return;
    const contents =
      format === 'csv'
        ? toDelimited(result.columns, result.rows)
        : format === 'json'
          ? toJSON(result.columns, result.rows)
          : format === 'md'
            ? toMarkdown(result.columns, result.rows)
            : toInserts(engine, '', '', result.columns, result.rows);
    try {
      await call('fs.writeText', { path, contents });
      toast('success', `Exported ${plural(result.rows.length, 'row')}`, path);
    } catch (e) {
      toast('error', 'Export failed', errorMessage(e));
    }
  };

  return (
    <div className="results">
      <div className="results-tabs">
        {outcomes?.map((o, i) => (
          <button key={i} className={clsx('results-tab', i === sel && 'is-active', o.error && 'is-error')} onClick={() => setSel(i)} title={o.sql}>
            {o.error ? <AlertTriangle size={12} /> : null}
            {label(o, i)}
            {o.result?.hasRows && <span className="results-tab-count">{formatCount(o.result.rows.length)}</span>}
          </button>
        ))}
        {outcomes && outcomes.length > 0 && (
          <button className={clsx('results-tab', sel === -1 && 'is-active')} onClick={() => setSel(-1)}>
            <MessageSquareText size={12} /> Messages
          </button>
        )}
        <div className="toolbar-spacer" />
        {running ? (
          <span className="results-status">
            <Spinner size={12} /> Running… {formatDuration(elapsed)}
          </span>
        ) : (
          totals && (
            <span className="results-status dim">
              {plural(totals.n, 'statement')} · {formatDuration(totals.ms)}
              {totals.errors > 0 && <span className="text-danger"> · {plural(totals.errors, 'error')}</span>}
            </span>
          )
        )}
      </div>
      <div className="results-body">
        {sel === -1 && outcomes ? (
          <div className="messages">
            {outcomes.map((o, i) => (
              <div key={i} className={clsx('message', o.error && 'is-error')}>
                <div className="message-head">
                  {o.error ? <AlertTriangle size={13} /> : <CheckCircle2 size={13} />}
                  <span>
                    {o.error
                      ? o.error.message
                      : o.result?.hasRows
                        ? `${plural(o.result.rows.length, 'row')} returned${o.result.truncated ? ' (truncated)' : ''}`
                        : `${plural(o.result?.rowsAffected ?? 0, 'row')} affected`}
                  </span>
                  {o.result && <span className="dim">{formatDuration(o.result.durationMs)}</span>}
                </div>
                <pre className="message-sql">{o.sql}</pre>
                {o.result?.notices.map((n, j) => (
                  <div key={j} className="notice">
                    {n}
                  </div>
                ))}
              </div>
            ))}
          </div>
        ) : current?.error ? (
          <div className="error-panel">
            <AlertTriangle size={18} />
            <div>
              <div className="error-title">
                {current.error.kind === 'cancelled' ? 'Query cancelled' : 'Query failed'}
                {current.error.code && <span className="error-code">{current.error.code}</span>}
              </div>
              <div className="error-msg">{current.error.message}</div>
              {current.error.detail && <div className="error-extra">Detail: {current.error.detail}</div>}
              {current.error.hint && <div className="error-extra">Hint: {current.error.hint}</div>}
              <pre className="error-sql">{current.sql}</pre>
              <div className="error-actions">
                {current.error.position && onShowError && (
                  <button className="link-btn" onClick={() => onShowError(current)}>
                    Show in editor
                  </button>
                )}
                {onFixWithAi && current.error.kind !== 'cancelled' && current.error.kind !== 'connection' && (
                  <button className="link-btn" onClick={() => onFixWithAi(current)}>
                    <Sparkles size={12} /> Fix with AI
                  </button>
                )}
              </div>
            </div>
          </div>
        ) : result?.hasRows ? (
          <>
            <DataGrid columns={result.columns} rows={result.rows} engine={engine} active={active} onActiveRowChange={setActiveRow} />
            <div className="result-footer">
              <span>
                {plural(result.rows.length, 'row')}
                {result.truncated && (
                  <span className="text-warn" title="Raise the limit in Settings → Editor">
                    {' '}
                    · truncated at the result limit
                  </span>
                )}
              </span>
              <span className="dim">{formatDuration(result.durationMs)}</span>
              <div className="toolbar-spacer" />
              <IconButton
                label="Copy all"
                onClick={(e) =>
                  openMenu(e, [
                    {
                      label: 'Copy as TSV (with headers)',
                      onSelect: () => copyText(toDelimited(result.columns, result.rows, '\t')).then(() => toast('success', 'Copied')),
                    },
                    {
                      label: 'Copy as CSV',
                      onSelect: () => copyText(toDelimited(result.columns, result.rows)).then(() => toast('success', 'Copied')),
                    },
                    {
                      label: 'Copy as JSON',
                      onSelect: () => copyText(toJSON(result.columns, result.rows)).then(() => toast('success', 'Copied')),
                    },
                    {
                      label: 'Copy as Markdown',
                      onSelect: () => copyText(toMarkdown(result.columns, result.rows)).then(() => toast('success', 'Copied')),
                    },
                  ])
                }
              >
                <Copy size={13} />
              </IconButton>
              <IconButton
                label="Export"
                onClick={(e) =>
                  openMenu(e, [
                    {
                      label: 'Export CSV…',
                      onSelect: () => exportResult('csv'),
                    },
                    {
                      label: 'Export JSON…',
                      onSelect: () => exportResult('json'),
                    },
                    {
                      label: 'Export SQL INSERTs…',
                      onSelect: () => exportResult('sql'),
                    },
                    {
                      label: 'Export Markdown…',
                      onSelect: () => exportResult('md'),
                    },
                  ])
                }
              >
                <Download size={13} />
              </IconButton>
            </div>
          </>
        ) : result ? (
          <div className="ok-panel">
            <CheckCircle2 size={20} />
            <div>
              <div className="ok-title">Query OK</div>
              <div className="dim">
                {plural(result.rowsAffected ?? 0, 'row')} affected · {formatDuration(result.durationMs)}
              </div>
              {result.notices.map((n, i) => (
                <div key={i} className="notice">
                  {n}
                </div>
              ))}
            </div>
          </div>
        ) : (
          <div className="center-fill">
            <Spinner size={18} />
          </div>
        )}
      </div>
      {menu}
    </div>
  );
}
