import clsx from 'clsx';
import { AlertTriangle, ArrowRight, Copy, CornerDownLeft, Settings2, Sparkles, X } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { copyText } from '../lib/platform';
import { call, errorMessage, RpcError } from '../lib/rpc';
import { dangerousReason, isWriteStatement, splitStatements } from '../lib/sqlsplit';
import type { AiJob, AiTask } from '../lib/types';
import { useStore, type Workspace } from '../state/store';
import { SqlEditor } from './SqlEditor';
import { Button, IconButton, Spinner } from './ui';

export interface AiRequest {
  task: AiTask;
  /** Generate: the request. Fix: an optional note. */
  question?: string;
  /** SQL to explain or fix. */
  sql?: string;
  error?: string;
  /** Editor range the SQL came from (fix replaces it). */
  range?: { from: number; to: number };
  /** Bumps to start a request from outside (Fix with AI, Explain). */
  nonce?: number;
}

interface StartReply {
  jobId: string;
  tables: number;
  provider: string;
  model: string;
  perDay: number | null;
  usedToday: number;
}

const PLACEHOLDERS = [
  'Ask for a query, e.g. top 10 customers by revenue this year',
  'e.g. orders with no matching payment, newest first',
  'e.g. how many users signed up each week',
];

/**
 * The AI bar above the SQL editor. It only ever puts SQL into the editor —
 * running it stays a separate, deliberate step with the usual safety checks.
 */
export function AiAssistant({
  ws,
  request,
  getEditorSql,
  onInsert,
  onReplaceAll,
  onReplaceRange,
  onClose,
}: {
  ws: Workspace;
  request: AiRequest;
  getEditorSql(): string;
  onInsert(sql: string): void;
  onReplaceAll(sql: string): void;
  onReplaceRange(range: { from: number; to: number }, sql: string): void;
  onClose(): void;
}) {
  const { openModal, toast, handleLimitError } = useStore.getState();
  const settings = useStore((s) => s.settings);
  const engine = ws.connection.engine;
  const [question, setQuestion] = useState(request.task === 'generate' ? (request.question ?? '') : '');
  const [task, setTask] = useState<AiTask>(request.task);
  const [job, setJob] = useState<AiJob | null>(null);
  const [meta, setMeta] = useState<StartReply | null>(null);
  const [error, setError] = useState<{
    message: string;
    setup?: boolean;
  } | null>(null);
  const jobId = useRef<string | null>(null);
  const timer = useRef<number | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const placeholder = useRef(PLACEHOLDERS[Math.floor(Math.random() * PLACEHOLDERS.length)]);
  const current = useRef(request);

  const stopPolling = () => {
    if (timer.current) window.clearInterval(timer.current);
    timer.current = null;
  };

  useEffect(
    () => () => {
      stopPolling();
      if (jobId.current) void call('ai.cancel', { jobId: jobId.current }).catch(() => {});
    },
    [],
  );

  const start = async (req: AiRequest) => {
    stopPolling();
    if (jobId.current) void call('ai.cancel', { jobId: jobId.current }).catch(() => {});
    current.current = req;
    setTask(req.task);
    setError(null);
    setJob({ status: 'thinking', elapsedMs: 0 });
    try {
      const r = await call<StartReply>('ai.start', {
        sessionId: ws.sessionId,
        task: req.task,
        question: req.question ?? '',
        sql: req.sql ?? '',
        error: req.error ?? '',
        schema: ws.schema,
      });
      setMeta(r);
      jobId.current = r.jobId;
      timer.current = window.setInterval(async () => {
        try {
          const j = await call<AiJob>('ai.poll', { jobId: r.jobId });
          setJob(j);
          if (j.status === 'done' || j.status === 'error' || j.status === 'cancelled') {
            stopPolling();
            if (j.status === 'error') setError({ message: j.error ?? 'Something went wrong' });
          }
        } catch (e) {
          stopPolling();
          setError({ message: errorMessage(e) });
          setJob(null);
        }
      }, 150);
    } catch (e) {
      setJob(null);
      if (handleLimitError(e)) return;
      setError({
        message: errorMessage(e),
        setup: e instanceof RpcError && e.code === 'ai_not_configured',
      });
    }
  };

  // Requests started from outside (Explain, Fix with AI) — once per nonce,
  // even if React runs the effect twice.
  const startedNonce = useRef<number | undefined>(undefined);
  useEffect(() => {
    if (startedNonce.current === request.nonce) return;
    startedNonce.current = request.nonce;
    if (request.task !== 'generate' && request.sql) void start(request);
    else setTimeout(() => inputRef.current?.focus(), 20);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [request.nonce]);

  const ask = () => {
    const q = question.trim();
    if (!q) return inputRef.current?.focus();
    void start({ task: 'generate', question: q, sql: getEditorSql() });
  };

  const cancel = () => {
    stopPolling();
    if (jobId.current) void call('ai.cancel', { jobId: jobId.current }).catch(() => {});
    jobId.current = null;
    setJob(null);
  };

  const busy = job?.status === 'thinking' || job?.status === 'writing';
  const done = job?.status === 'done';
  const sql = (job?.sql ?? '').trim();
  const stmts = sql ? splitStatements(sql, engine) : [];
  const writes = stmts.some((s) => isWriteStatement(s.text, engine));
  const danger = stmts.map((s) => dangerousReason(s.text, engine)).find(Boolean);
  const readOnly = ws.info.readOnly || ws.connection.readOnly;
  const seconds = ((job?.elapsedMs ?? 0) / 1000).toFixed(1);
  const fixRange = current.current.task === 'fix' ? current.current.range : undefined;

  // "claude-haiku-5-5" → "Claude Haiku 5.5"; other providers' model IDs are shown as they are.
  const providerName =
    meta?.provider === 'anthropic'
      ? meta.model
          .replace('claude-', 'Claude ')
          .replace(/-(\d)-(\d)$/, ' $1.$2')
          .replace(/(^|\s)(\w)/g, (m) => m.toUpperCase())
      : (meta?.model ?? '');

  return (
    <div className="ai-bar" onKeyDown={(e) => e.key === 'Escape' && (busy ? cancel() : onClose())}>
      <form
        className="ai-ask"
        onSubmit={(e) => {
          e.preventDefault();
          ask();
        }}
      >
        <Sparkles size={15} className="ai-icon" />
        <input
          ref={inputRef}
          // Focus right away so typing straight after ⌘I lands here, not in the editor.
          autoFocus={request.task === 'generate'}
          className="ai-input"
          value={question}
          placeholder={
            task === 'explain'
              ? 'Explaining the selected SQL…'
              : task === 'fix'
                ? 'Fixing the failed statement… (add a note and press Enter to retry)'
                : placeholder.current
          }
          onChange={(e) => setQuestion(e.target.value)}
          spellCheck={false}
        />
        {busy ? (
          <Button size="sm" onClick={cancel}>
            Stop
          </Button>
        ) : (
          <Button size="sm" variant="primary" type="submit" disabled={!question.trim()}>
            Ask <CornerDownLeft size={11} />
          </Button>
        )}
        <IconButton label="AI settings" onClick={() => openModal({ kind: 'settings', section: 'ai' })}>
          <Settings2 size={14} />
        </IconButton>
        <IconButton label="Close (Esc)" onClick={onClose}>
          <X size={14} />
        </IconButton>
      </form>

      {busy && (
        <div className="ai-status">
          <Spinner size={12} />
          <span>
            {job?.status === 'writing' ? 'Writing' : 'Thinking'}… {seconds}s
            {meta ? (
              <span className="dim">
                {' '}
                · {meta.tables} tables in context · {providerName}
              </span>
            ) : null}
          </span>
        </div>
      )}

      {error && (
        <div className="ai-error">
          <AlertTriangle size={14} />
          <span>{error.message}</span>
          {error.setup && (
            <Button size="sm" onClick={() => openModal({ kind: 'settings', section: 'ai' })}>
              Set up AI <ArrowRight size={11} />
            </Button>
          )}
        </div>
      )}

      {(busy || done) && (sql || job?.explanation) ? (
        <div className={clsx('ai-result', busy && 'is-streaming')}>
          {sql && task !== 'explain' ? (
            <div className="ai-sql">
              <SqlEditor value={sql} engine={engine} readOnly minimal fontSize={settings.editorFontSize} />
            </div>
          ) : null}
          {job?.explanation ? <div className="ai-explanation">{job.explanation}</div> : null}
          {done && (
            <div className="ai-actions">
              {sql && task !== 'explain' ? (
                <>
                  {fixRange ? (
                    <Button size="sm" variant="primary" onClick={() => onReplaceRange(fixRange, sql)}>
                      Replace the statement
                    </Button>
                  ) : (
                    <Button size="sm" variant="primary" onClick={() => onInsert(sql)}>
                      Insert into editor
                    </Button>
                  )}
                  <Button size="sm" onClick={() => onReplaceAll(sql)}>
                    Replace editor
                  </Button>
                  <IconButton
                    label="Copy SQL"
                    onClick={() => {
                      void copyText(sql);
                      toast('success', 'Copied');
                    }}
                  >
                    <Copy size={13} />
                  </IconButton>
                </>
              ) : null}
              {writes && (
                <span className={clsx('tag', danger ? 'tag-danger' : 'tag-warn')} title={danger ?? undefined}>
                  <AlertTriangle size={10} />{' '}
                  {danger ? 'Destructive — review carefully' : readOnly ? 'Changes data — blocked on this read-only connection' : 'Changes data'}
                </span>
              )}
              <span className="toolbar-spacer" />
              <span className="dim small">
                Review before running · {providerName}
                {job?.outputTokens ? ` · ${job.inputTokens ?? '?'} in / ${job.outputTokens} out tokens` : ''}
                {meta?.perDay != null ? ` · ${Math.max(0, meta.perDay - meta.usedToday)} free left today` : ''}
              </span>
            </div>
          )}
        </div>
      ) : done && !sql && !job?.explanation ? (
        <div className="ai-error">
          <AlertTriangle size={14} />
          <span>The model returned an empty answer. Try rephrasing.</span>
        </div>
      ) : null}
    </div>
  );
}
