import clsx from 'clsx';
import { FileUp, FolderOpen, XCircle } from 'lucide-react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { compactCount, uid } from '../lib/format';
import { pickOpenFile } from '../lib/platform';
import { call, errorMessage } from '../lib/rpc';
import type { TableStructure } from '../lib/types';
import { useStore, type Workspace } from '../state/store';
import { Button, Field, Modal, Segmented, Spinner, Toggle } from './ui';

interface CsvColumn {
  name: string;
  kind: string;
}

interface CsvPreview {
  path: string;
  delimiter: string;
  hasHeader: boolean;
  encoding: string;
  columns: CsvColumn[];
  rows: string[][];
  totalRows: number;
  fileSize: number;
}

interface NewCol {
  include: boolean;
  name: string;
  sqlType: string;
}

const SKIP = '';

function tableNameFromFile(path: string): string {
  const base = (path.split(/[\\/]/).pop() ?? 'imported').replace(/\.[^.]+$/, '');
  const clean = base.replace(/[^A-Za-z0-9_]+/g, '_').replace(/^_+|_+$/g, '').toLowerCase();
  return /^[a-z_]/.test(clean) ? clean || 'imported' : `t_${clean}`;
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export function ImportModal({ ws, table: initialTable }: { ws: Workspace; table?: string }) {
  const { openModal, toast, loadObjects, openTable, bumpRefresh, handleLimitError } = useStore.getState();
  const license = useStore((s) => s.license);
  const engine = ws.connection.engine;
  const tables = useMemo(() => ws.objects.filter((o) => o.kind === 'table'), [ws.objects]);
  const [preview, setPreview] = useState<CsvPreview | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [mode, setMode] = useState<'existing' | 'new'>(initialTable ? 'existing' : 'new');
  const [table, setTable] = useState(initialTable ?? tables[0]?.name ?? '');
  const [newName, setNewName] = useState('');
  const [structure, setStructure] = useState<TableStructure | null>(null);
  const [mapping, setMapping] = useState<string[]>([]);
  const [newCols, setNewCols] = useState<NewCol[]>([]);
  const [emptyAsNull, setEmptyAsNull] = useState(true);
  const [job, setJob] = useState<{ id: string; done: number; total: number } | null>(null);
  const started = useRef(false);

  const load = useCallback(
    async (path: string, opts: { delimiter?: string; hasHeader?: boolean } = {}) => {
      setLoading(true);
      setError(null);
      try {
        const p = await call<CsvPreview>('import.preview', { path, delimiter: opts.delimiter ?? null, hasHeader: opts.hasHeader ?? null });
        setPreview(p);
        const kinds = [...new Set(p.columns.map((c) => c.kind))];
        const types = await Promise.all(kinds.map((kind) => call<string>('import.sqlType', { engine, kind })));
        const typeOf = new Map(kinds.map((k, i) => [k, types[i]]));
        setNewCols(p.columns.map((c) => ({ include: true, name: c.name, sqlType: typeOf.get(c.kind) ?? '' })));
        setNewName((n) => n || tableNameFromFile(path));
      } catch (e) {
        setError(errorMessage(e));
      } finally {
        setLoading(false);
      }
    },
    [engine],
  );

  const chooseFile = useCallback(async () => {
    const path = await pickOpenFile('Import CSV', [
      { name: 'CSV / TSV', extensions: ['csv', 'tsv', 'txt', 'tab'] },
      { name: 'All files', extensions: ['*'] },
    ]);
    if (path) await load(path);
    else if (!preview) openModal(null);
  }, [load, openModal, preview]);

  useEffect(() => {
    if (started.current) return;
    started.current = true;
    void chooseFile();
  }, [chooseFile]);

  // Load the target table's columns and match them to the CSV by name.
  useEffect(() => {
    if (mode !== 'existing' || !table) return;
    let alive = true;
    call<TableStructure>('meta.structure', { sessionId: ws.sessionId, schema: ws.schema, table })
      .then((st) => {
        if (!alive) return;
        setStructure(st);
      })
      .catch((e) => alive && setError(errorMessage(e)));
    return () => {
      alive = false;
    };
  }, [mode, table, ws.sessionId, ws.schema]);

  useEffect(() => {
    if (!preview || !structure) return;
    const names = structure.columns.map((c) => c.name);
    setMapping(
      preview.columns.map((c) => names.find((n) => n.toLowerCase() === c.name.trim().toLowerCase()) ?? SKIP),
    );
  }, [preview, structure]);

  const overLimit = !license?.licensed && license?.limits.importRows != null && (preview?.totalRows ?? 0) > license.limits.importRows;

  const columnsToSend = () => {
    if (!preview) return [];
    if (mode === 'existing') {
      const types = new Map((structure?.columns ?? []).map((c) => [c.name, c.dataType]));
      return mapping
        .map((target, source) => ({ source, target, sqlType: types.get(target) ?? '' }))
        .filter((m) => m.target !== SKIP);
    }
    return newCols.map((c, source) => ({ source, target: c.name.trim(), sqlType: c.sqlType.trim(), include: c.include })).filter((c) => c.include);
  };

  const duplicateTargets = (() => {
    const t = columnsToSend().map((c) => c.target.toLowerCase());
    return t.length !== new Set(t).size;
  })();

  const runImport = async () => {
    if (!preview) return;
    if (overLimit) {
      handleLimitError({ kind: 'license', message: `The free plan imports up to ${license?.limits.importRows?.toLocaleString()} rows at a time; this file has ${preview.totalRows.toLocaleString()}. Upgrade to Rowglow Pro for unlimited imports.` } as never);
      return;
    }
    const target = mode === 'existing' ? table : newName.trim();
    const jobId = uid();
    setJob({ id: jobId, done: 0, total: preview.totalRows });
    setError(null);
    const timer = window.setInterval(async () => {
      try {
        const p = await call<{ done: number; total: number }>('import.progress', { jobId });
        setJob((j) => (j && j.id === jobId ? { ...j, done: p.done, total: p.total || j.total } : j));
      } catch {
        /* progress is best-effort */
      }
    }, 300);
    try {
      const res = await call<{ rows: number }>('import.run', {
        sessionId: ws.sessionId,
        jobId,
        path: preview.path,
        delimiter: preview.delimiter,
        hasHeader: preview.hasHeader,
        encoding: preview.encoding,
        schema: ws.schema,
        table: target,
        create: mode === 'new',
        columns: columnsToSend().map(({ source, target, sqlType }) => ({ source, target, sqlType })),
        emptyAsNull,
      });
      openModal(null);
      toast('success', `Imported ${res.rows.toLocaleString()} rows`, `into ${target}`);
      await loadObjects(ws.sessionId);
      bumpRefresh(ws.sessionId);
      openTable(ws.sessionId, ws.schema, { name: target, kind: 'table' });
    } catch (e) {
      if (!handleLimitError(e)) setError(errorMessage(e));
    } finally {
      window.clearInterval(timer);
      setJob(null);
    }
  };

  const canImport =
    !!preview &&
    !job &&
    !duplicateTargets &&
    columnsToSend().length > 0 &&
    (mode === 'existing' ? !!table : !!newName.trim() && newCols.every((c) => !c.include || c.name.trim()));

  const pct = job && job.total ? Math.min(100, Math.round((job.done / job.total) * 100)) : 0;

  return (
    <Modal
      title={
        <span className="import-title">
          <FileUp size={15} /> Import CSV
        </span>
      }
      onClose={() => (job ? undefined : openModal(null))}
      width={760}
      className="import-modal"
      footer={
        job ? (
          <>
            <div className="import-progress">
              <div className="import-bar">
                <span style={{ width: `${pct}%` }} />
              </div>
              <span className="dim">
                {job.done.toLocaleString()} / {job.total.toLocaleString()} rows · one transaction, nothing is saved until the end
              </span>
            </div>
            <Button onClick={() => call('import.cancel', { jobId: job.id })}>Cancel</Button>
          </>
        ) : (
          <>
            <span className="dim import-meta">
              {preview
                ? `${preview.totalRows.toLocaleString()} rows · ${formatBytes(preview.fileSize)} · ${preview.encoding.toUpperCase()}`
                : ''}
            </span>
            <div className="toolbar-spacer" />
            <Button variant="ghost" onClick={() => openModal(null)}>
              Close
            </Button>
            <Button variant="primary" disabled={!canImport} onClick={runImport}>
              Import {preview ? compactCount(preview.totalRows) : ''} rows
            </Button>
          </>
        )
      }
    >
      {!preview && loading && (
        <div className="import-loading">
          <Spinner /> Reading file…
        </div>
      )}
      {!preview && !loading && (
        <div className="import-loading">
          <Button onClick={chooseFile}>
            <FolderOpen size={13} /> Choose a CSV file…
          </Button>
        </div>
      )}
      {preview && (
        <div className="import-body">
          <div className="import-file">
            <span className="mono" title={preview.path}>
              {preview.path.split(/[\\/]/).pop()}
            </span>
            <Button size="sm" variant="ghost" onClick={chooseFile} disabled={!!job}>
              Change…
            </Button>
          </div>

          <div className="form-grid import-options">
            <Field label="Delimiter">
              <select
                className="select"
                value={preview.delimiter}
                disabled={!!job}
                onChange={(e) => load(preview.path, { delimiter: e.target.value, hasHeader: preview.hasHeader })}
              >
                <option value=",">Comma ,</option>
                <option value=";">Semicolon ;</option>
                <option value="\t">Tab</option>
                <option value="|">Pipe |</option>
              </select>
            </Field>
            <Field label="First row">
              <Segmented
                value={preview.hasHeader ? 'header' : 'data'}
                onChange={(v) => load(preview.path, { delimiter: preview.delimiter, hasHeader: v === 'header' })}
                options={[
                  { value: 'header', label: 'Column names' },
                  { value: 'data', label: 'Data' },
                ]}
              />
            </Field>
            <Field label="Empty cells">
              <Toggle checked={emptyAsNull} onChange={setEmptyAsNull} label="Import as NULL" />
            </Field>
          </div>

          <Field label="Into">
            <Segmented
              value={mode}
              onChange={setMode}
              options={[
                { value: 'new', label: 'A new table' },
                { value: 'existing', label: 'An existing table', title: tables.length ? undefined : 'No tables in this schema' },
              ]}
            />
          </Field>

          {mode === 'new' ? (
            <>
              <Field label={`New table in ${ws.schema}`}>
                <input className="input mono" value={newName} onChange={(e) => setNewName(e.target.value)} spellCheck={false} />
              </Field>
              <div className="import-map">
                <div className="import-map-head">
                  <span />
                  <span>CSV column</span>
                  <span>Column name</span>
                  <span>Type</span>
                </div>
                {newCols.map((c, i) => (
                  <div key={i} className={clsx('import-map-row', !c.include && 'is-skipped')}>
                    <input
                      type="checkbox"
                      checked={c.include}
                      onChange={(e) => setNewCols((cols) => cols.map((x, j) => (j === i ? { ...x, include: e.target.checked } : x)))}
                      aria-label={`Include ${preview.columns[i]?.name}`}
                    />
                    <span className="mono ellipsis" title={preview.columns[i]?.name}>
                      {preview.columns[i]?.name}
                    </span>
                    <input
                      className="input input-sm mono"
                      value={c.name}
                      disabled={!c.include}
                      onChange={(e) => setNewCols((cols) => cols.map((x, j) => (j === i ? { ...x, name: e.target.value } : x)))}
                      spellCheck={false}
                    />
                    <input
                      className="input input-sm mono"
                      value={c.sqlType}
                      disabled={!c.include}
                      onChange={(e) => setNewCols((cols) => cols.map((x, j) => (j === i ? { ...x, sqlType: e.target.value } : x)))}
                      spellCheck={false}
                    />
                  </div>
                ))}
              </div>
            </>
          ) : (
            <>
              <Field label="Table">
                <select className="select" value={table} onChange={(e) => setTable(e.target.value)}>
                  {tables.map((t) => (
                    <option key={t.name} value={t.name}>
                      {t.name}
                    </option>
                  ))}
                </select>
              </Field>
              <div className="import-map">
                <div className="import-map-head is-existing">
                  <span>CSV column</span>
                  <span>Goes into</span>
                  <span>Sample</span>
                </div>
                {preview.columns.map((c, i) => (
                  <div key={i} className={clsx('import-map-row is-existing', mapping[i] === SKIP && 'is-skipped')}>
                    <span className="mono ellipsis" title={c.name}>
                      {c.name}
                    </span>
                    <select
                      className="select select-sm"
                      value={mapping[i] ?? SKIP}
                      onChange={(e) => setMapping((m) => m.map((x, j) => (j === i ? e.target.value : x)))}
                    >
                      <option value={SKIP}>Skip</option>
                      {(structure?.columns ?? []).map((col) => (
                        <option key={col.name} value={col.name}>
                          {col.name} · {col.dataType}
                        </option>
                      ))}
                    </select>
                    <span className="mono dim ellipsis">{preview.rows[0]?.[i] ?? ''}</span>
                  </div>
                ))}
              </div>
            </>
          )}

          <div className="import-preview">
            <table>
              <thead>
                <tr>
                  {preview.columns.map((c, i) => (
                    <th key={i}>{c.name}</th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {preview.rows.slice(0, 8).map((r, i) => (
                  <tr key={i}>
                    {preview.columns.map((_, j) => (
                      <td key={j} className={r[j] === '' || r[j] === undefined ? 'is-empty' : undefined}>
                        {r[j] === '' || r[j] === undefined ? (emptyAsNull ? 'NULL' : '') : r[j]}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          {overLimit && (
            <div className="callout">
              The free plan imports up to {license?.limits.importRows?.toLocaleString()} rows at a time. Pro imports files of any size.
            </div>
          )}
          {duplicateTargets && <div className="callout callout-error">Two CSV columns go into the same table column.</div>}
        </div>
      )}
      {error && (
        <div className="callout callout-error import-error">
          <XCircle size={14} />
          <span>{error}</span>
        </div>
      )}
    </Modal>
  );
}
