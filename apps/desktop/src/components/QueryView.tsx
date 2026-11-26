import type { EditorView } from '@codemirror/view';
import { ChevronDown, Play, Square, Wand2, Save, Gauge, FileDown, Sparkles } from 'lucide-react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { format as formatSql } from 'sql-formatter';
import { useHotkeys } from '../lib/hotkeys';
import { mod, pickSaveFile } from '../lib/platform';
import { call, errorMessage } from '../lib/rpc';
import { dangerousReason, isWriteStatement, splitStatements, statementAt, type Stmt } from '../lib/sqlsplit';
import type { StatementOutcome } from '../lib/types';
import { useStore, type QueryTab, type Workspace } from '../state/store';
import { AiAssistant, type AiRequest } from './AiAssistant';
import { ResultsPanel } from './ResultsPanel';
import { setErrorMark, SqlEditor, type RunMode } from './SqlEditor';
import { Button, IconButton, Spinner, Splitter, useMenu } from './ui';

const DDL = /^\s*(CREATE|ALTER|DROP|RENAME|TRUNCATE|COMMENT)\b/i;

export function QueryView({ ws, tab, active }: { ws: Workspace; tab: QueryTab; active: boolean }) {
  const engine = ws.connection.engine;
  const settings = useStore((s) => s.settings);
  const { toast, updateTab, confirm, openModal } = useStore.getState();
  const [sql, setSql] = useState(tab.sql);
  const [outcomes, setOutcomes] = useState<StatementOutcome[] | null>(null);
  const [running, setRunning] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const [editorHeight, setEditorHeight] = useState<number>(() => Math.round(window.innerHeight * 0.38));
  const viewRef = useRef<EditorView | null>(null);
  const lastStmts = useRef<Stmt[]>([]);
  const [menu, openMenu] = useMenu();
  const sqlRef = useRef(sql);
  sqlRef.current = sql;
  const [ai, setAi] = useState<AiRequest | null>(null);
  const aiNonce = useRef(0);
  const aiOpen = (req: AiRequest) => setAi({ ...req, nonce: ++aiNonce.current });

  // Persist editor text into the tab (debounced) so it survives tab switches & palette search.
  useEffect(() => {
    const t = setTimeout(() => {
      if (sql !== tab.sql) updateTab(ws.sessionId, tab.id, { sql, dirty: !!tab.snippetId });
    }, 400);
    return () => clearTimeout(t);
  }, [sql, tab.sql, tab.id, tab.snippetId, ws.sessionId, updateTab]);

  // External updates to tab.sql (history "insert", open snippet)
  useEffect(() => {
    if (tab.sql !== sqlRef.current) setSql(tab.sql);
  }, [tab.sql]);

  useEffect(() => {
    if (!running) return;
    const start = performance.now();
    const t = setInterval(() => setElapsed(performance.now() - start), 100);
    return () => clearInterval(t);
  }, [running]);

  const pickStatements = useCallback(
    (mode: RunMode): Stmt[] => {
      const view = viewRef.current;
      const text = view ? view.state.doc.toString() : sqlRef.current;
      if (view) {
        const sel = view.state.selection.main;
        if (!sel.empty) {
          const part = text.slice(sel.from, sel.to);
          return splitStatements(part, engine).map((s) => ({
            ...s,
            from: s.from + sel.from,
            to: s.to + sel.from,
            end: s.end + sel.from,
          }));
        }
      }
      const all = splitStatements(text, engine);
      if (mode === 'all') return all;
      const pos = view ? view.state.selection.main.head : text.length;
      const s = statementAt(all, pos, text);
      return s ? [s] : [];
    },
    [engine],
  );

  const execute = useCallback(
    async (stmts: Stmt[], explain = false) => {
      if (running) return;
      if (!stmts.length) {
        const empty = !(viewRef.current?.state.doc.toString() ?? sqlRef.current).trim();
        toast(
          'info',
          'Nothing to run',
          empty ? 'Write a statement first.' : `Put the cursor inside a statement or select the SQL to run. Run all with ⇧${mod}↵.`,
        );
        return;
      }
      let texts = stmts.map((s) => s.text);
      if (explain) {
        if (engine === 'mssql') {
          toast('info', 'Explain isn’t available for SQL Server yet', 'Use SET SHOWPLAN_TEXT ON in its own statement, then run the query.');
          return;
        }
        const prefix = engine === 'sqlite' ? 'EXPLAIN QUERY PLAN ' : 'EXPLAIN ';
        texts = [prefix + texts[0]];
        stmts = [stmts[0]];
      }
      const writes = texts.filter((t) => isWriteStatement(t, engine));
      const dangers = explain ? [] : texts.map((t) => ({ t, why: dangerousReason(t, engine) })).filter((d) => d.why);
      const prodWrites = ws.connection.env === 'production' && settings.confirmProdWrites && writes.length > 0 && !explain;
      if (prodWrites || (dangers.length && settings.confirmDangerous)) {
        const shown = prodWrites ? writes : dangers.map((d) => d.t);
        const ok = await confirm({
          title: prodWrites
            ? `Run ${writes.length === 1 ? 'a write statement' : `${writes.length} write statements`} on production?`
            : dangers.length === 1
              ? 'Run a destructive statement?'
              : `Run ${dangers.length} destructive statements?`,
          message: [
            prodWrites ? `${ws.connection.name} is tagged as production.` : '',
            ...dangers.map((d) => `⚠︎ ${d.why}.`),
            'Double-check before running:',
          ]
            .filter(Boolean)
            .join(' '),
          code: shown.join(';\n') + ';',
          confirmLabel: prodWrites ? 'Run on production' : 'Run anyway',
          danger: true,
        });
        if (!ok) return;
      }
      viewRef.current?.dispatch({ effects: setErrorMark.of(null) });
      lastStmts.current = stmts;
      setRunning(true);
      setElapsed(0);
      try {
        const res = await call<StatementOutcome[]>('query.execute', {
          sessionId: ws.sessionId,
          statements: texts,
          limit: settings.resultLimit,
          recordHistory: true,
          stopOnError: true,
        });
        setOutcomes(res);
        const err = res.find((o) => o.error);
        if (err) {
          const idx = res.indexOf(err);
          markError(stmts[idx], err.error?.position);
        }
        if (texts.some((t) => DDL.test(t))) {
          const st = useStore.getState();
          void st.loadObjects(ws.sessionId);
          void st.loadCompletions(ws.sessionId);
          st.bumpRefresh(ws.sessionId);
        } else if (writes.length) {
          useStore.getState().bumpRefresh(ws.sessionId);
        }
      } catch (e) {
        toast('error', 'Query failed', errorMessage(e));
      } finally {
        setRunning(false);
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [running, engine, ws.sessionId, ws.connection.env, ws.connection.name, settings.resultLimit, settings.confirmProdWrites],
  );

  const markError = (stmt: Stmt | undefined, position?: number) => {
    const view = viewRef.current;
    if (!view || !stmt) return;
    if (!position) {
      view.dispatch({
        effects: setErrorMark.of({ from: stmt.from, to: stmt.to }),
      });
      return;
    }
    // Postgres positions are 1-based character offsets into the statement text.
    const start = stmt.from + position - 1;
    const docLen = view.state.doc.length;
    let end = start;
    const text = view.state.doc.toString();
    while (end < docLen && /[\w."]/.test(text[end])) end++;
    if (end === start) end = Math.min(docLen, start + 1);
    view.dispatch({
      effects: setErrorMark.of({
        from: Math.min(start, docLen),
        to: Math.min(end, docLen),
      }),
    });
  };

  const run = useCallback((mode: RunMode) => void execute(pickStatements(mode)), [execute, pickStatements]);

  // ---- AI assistant: it only ever edits the editor; running stays with the user.
  const editorText = () => viewRef.current?.state.doc.toString() ?? sqlRef.current;
  const askAi = () => aiOpen({ task: 'generate' });
  const explainWithAi = () => {
    const stmts = pickStatements('current');
    const text = stmts.map((s) => s.text).join(';\n');
    if (!text.trim()) {
      toast('info', 'Nothing to explain', 'Put the cursor inside a statement or select some SQL.');
      return;
    }
    aiOpen({ task: 'explain', sql: text });
  };
  const insertSql = (text: string) => {
    const view = viewRef.current;
    if (!view) return setSql((cur) => (cur.trim() ? `${cur.trimEnd()}\n\n${text}\n` : `${text}\n`));
    const sel = view.state.selection.main;
    const doc = view.state.doc.toString();
    let prefix = '';
    let suffix = '';
    let from = sel.from;
    let to = sel.to;
    if (sel.empty) {
      // On an empty line: insert there. Otherwise add it after everything
      // else, so it never lands inside an existing statement.
      const line = view.state.doc.lineAt(sel.head);
      from = to = line.text.trim() ? doc.length : line.from;
      const before = doc.slice(0, from);
      prefix = before.trim() ? (before.endsWith('\n\n') ? '' : before.endsWith('\n') ? '\n' : '\n\n') : '';
      if (!/;\s*$/.test(text) && engine !== 'mssql') suffix = ';';
    }
    const insert = prefix + text + suffix;
    // Select the new SQL so ⌘↵ runs exactly it.
    view.dispatch({
      changes: { from, to, insert },
      selection: { anchor: from + prefix.length, head: from + insert.length },
    });
    view.focus();
    setAi(null);
  };
  const replaceAllSql = (text: string) => {
    const view = viewRef.current;
    if (view) {
      view.dispatch({
        changes: { from: 0, to: view.state.doc.length, insert: text },
        selection: { anchor: 0 },
      });
      view.focus();
    } else setSql(text);
    setAi(null);
  };
  const replaceRange = (range: { from: number; to: number }, text: string) => {
    const view = viewRef.current;
    if (!view) return replaceAllSql(text);
    const len = view.state.doc.length;
    const from = Math.min(range.from, len);
    const to = Math.min(range.to, len);
    view.dispatch({
      changes: { from, to, insert: text.replace(/;\s*$/, '') },
      effects: setErrorMark.of(null),
      selection: { anchor: from },
    });
    view.focus();
    setAi(null);
  };
  const explain = () => void execute(pickStatements('current'), true);

  const cancel = async () => {
    try {
      await call('query.cancel', { sessionId: ws.sessionId });
    } catch (e) {
      toast('error', 'Couldn’t cancel', errorMessage(e));
    }
  };

  const doFormat = () => {
    const view = viewRef.current;
    const text = view ? view.state.doc.toString() : sql;
    const language =
      engine === 'postgres'
        ? 'postgresql'
        : engine === 'mysql'
          ? /mariadb/i.test(ws.info.flavor)
            ? 'mariadb'
            : 'mysql'
          : engine === 'mssql'
            ? 'transactsql'
            : engine === 'duckdb'
              ? 'duckdb'
              : 'sqlite';
    try {
      const sel = view?.state.selection.main;
      if (view && sel && !sel.empty) {
        const formatted = formatSql(text.slice(sel.from, sel.to), {
          language,
          keywordCase: 'upper',
          tabWidth: 2,
        });
        view.dispatch({
          changes: { from: sel.from, to: sel.to, insert: formatted },
        });
      } else {
        setSql(
          formatSql(text, {
            language,
            keywordCase: 'upper',
            tabWidth: 2,
            linesBetweenQueries: 2,
          }),
        );
      }
    } catch (e) {
      toast('error', 'Couldn’t format', errorMessage(e));
    }
  };

  const save = () => {
    const text = viewRef.current?.state.doc.toString() ?? sql;
    if (!text.trim()) return;
    const snip = tab.snippetId ? useStore.getState().snippets.find((s) => s.id === tab.snippetId) : undefined;
    if (snip) {
      call('snippets.save', { snippet: { ...snip, sql: text } })
        .then(() => {
          void useStore.getState().loadSnippets();
          updateTab(ws.sessionId, tab.id, { dirty: false, sql: text });
          toast('success', `Saved “${snip.name}”`);
        })
        .catch((e) => toast('error', 'Couldn’t save', errorMessage(e)));
      return;
    }
    openModal({
      kind: 'saveSnippet',
      sessionId: ws.sessionId,
      tabId: tab.id,
      sql: text,
      name: tab.title.startsWith('Query ') ? '' : tab.title,
    });
  };

  const saveFile = async () => {
    const text = viewRef.current?.state.doc.toString() ?? sql;
    const path = await pickSaveFile('Save SQL file', `${tab.title.replace(/[^\w.-]+/g, '_')}.sql`, [{ name: 'SQL', extensions: ['sql'] }]);
    if (!path) return;
    try {
      await call('fs.writeText', { path, contents: text });
      updateTab(ws.sessionId, tab.id, {
        filePath: path,
        title: path.split('/').pop() ?? tab.title,
      });
      toast('success', 'Saved', path);
    } catch (e) {
      toast('error', 'Couldn’t save file', errorMessage(e));
    }
  };

  // Auto-run (e.g. "Run" from history or palette)
  useEffect(() => {
    if (tab.autoRun && viewRef.current) {
      updateTab(ws.sessionId, tab.id, { autoRun: false });
      setTimeout(() => run('all'), 50);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab.autoRun]);

  useEffect(() => {
    if (tab.askAi) {
      updateTab(ws.sessionId, tab.id, { askAi: false });
      askAi();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab.askAi]);

  useHotkeys(active, {
    'mod+i': () => askAi(),
    'mod+.': () => running && void cancel(),
    'mod+enter': () => run('current'),
    'mod+shift+enter': () => run('all'),
  });

  useEffect(() => {
    if (active) setTimeout(() => viewRef.current?.focus(), 30);
  }, [active]);

  return (
    <div className="query-view">
      <div className="toolbar">
        <div className="btn-split">
          <Button size="sm" variant="primary" onClick={() => run('current')} disabled={running} title={`Run statement at cursor (${mod}↵)`}>
            <Play size={12} fill="currentColor" /> Run
          </Button>
          <Button
            size="sm"
            variant="primary"
            className="btn-split-caret"
            disabled={running}
            onClick={(e) =>
              openMenu(e, [
                {
                  label: 'Run statement at cursor',
                  shortcut: `${mod}↵`,
                  onSelect: () => run('current'),
                },
                {
                  label: 'Run all statements',
                  shortcut: `⇧${mod}↵`,
                  onSelect: () => run('all'),
                },
                { label: 'Explain statement', onSelect: explain },
                {
                  label: 'Explain with AI',
                  shortcut: '',
                  onSelect: explainWithAi,
                },
              ])
            }
          >
            <ChevronDown size={12} />
          </Button>
        </div>
        {running && (
          <Button size="sm" variant="danger" onClick={cancel} title={`Cancel (${mod}.)`}>
            <Square size={10} fill="currentColor" /> Stop
          </Button>
        )}
        <IconButton label="Explain plan" onClick={explain} disabled={running}>
          <Gauge size={15} />
        </IconButton>
        <IconButton label={`Format SQL (⇧${mod}F)`} onClick={doFormat}>
          <Wand2 size={15} />
        </IconButton>
        <IconButton label={`Save query (${mod}S)`} onClick={save}>
          <Save size={15} />
        </IconButton>
        <IconButton label="Save as .sql file…" onClick={saveFile}>
          <FileDown size={15} />
        </IconButton>
        <Button
          size="sm"
          variant="ghost"
          className={ai ? 'is-active' : undefined}
          onClick={() => (ai ? setAi(null) : askAi())}
          title={`Ask AI (${mod}I)`}
        >
          <Sparkles size={13} /> Ask AI
        </Button>
        <div className="toolbar-spacer" />
        {running && <Spinner size={13} />}
        <span className="toolbar-note mono">
          {ws.info.database}
          {engine !== 'sqlite' && ws.schema && engine === 'postgres' ? ` · ${ws.schema}` : ''}
        </span>
      </div>
      {ai && (
        <AiAssistant
          ws={ws}
          request={ai}
          getEditorSql={editorText}
          onInsert={insertSql}
          onReplaceAll={replaceAllSql}
          onReplaceRange={replaceRange}
          onClose={() => {
            setAi(null);
            viewRef.current?.focus();
          }}
        />
      )}
      <div className="query-editor" style={{ height: editorHeight }}>
        <SqlEditor
          value={sql}
          engine={engine}
          onChange={setSql}
          completions={ws.completions}
          defaultSchema={ws.schema}
          fontSize={settings.editorFontSize}
          autoFocus={active}
          placeholder={`-- Write SQL here. ${mod}↵ runs the statement under the cursor.`}
          onRun={run}
          onFormat={doFormat}
          onSave={save}
          onAsk={askAi}
          onReady={(v) => (viewRef.current = v)}
        />
      </div>
      <Splitter direction="horizontal" onDrag={(d) => setEditorHeight((h) => Math.max(80, Math.min(window.innerHeight - 200, h + d)))} />
      <div className="query-results">
        <ResultsPanel
          outcomes={outcomes}
          running={running}
          elapsed={elapsed}
          engine={engine}
          active={active}
          tabId={tab.id}
          onShowError={(o) => {
            const idx = outcomes?.indexOf(o) ?? -1;
            markError(lastStmts.current[idx], o.error?.position);
            viewRef.current?.focus();
          }}
          onFixWithAi={(o) => {
            const idx = outcomes?.indexOf(o) ?? -1;
            const stmt = lastStmts.current[idx];
            const err = o.error;
            const message = [err?.message, err?.detail && `Detail: ${err.detail}`, err?.hint && `Hint: ${err.hint}`].filter(Boolean).join('\n');
            aiOpen({
              task: 'fix',
              sql: o.sql,
              error: message,
              range: stmt ? { from: stmt.from, to: stmt.to } : undefined,
            });
          }}
        />
      </div>
      {menu}
    </div>
  );
}
