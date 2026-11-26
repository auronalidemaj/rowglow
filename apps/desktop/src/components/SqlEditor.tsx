import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap, type Completion } from '@codemirror/autocomplete';
import { defaultKeymap, history, historyKeymap, indentWithTab, toggleComment } from '@codemirror/commands';
import { MSSQL, MySQL, PostgreSQL, SQLite, sql, type SQLNamespace } from '@codemirror/lang-sql';
import { bracketMatching, HighlightStyle, indentOnInput, syntaxHighlighting } from '@codemirror/language';
import { highlightSelectionMatches, search, searchKeymap } from '@codemirror/search';
import { Compartment, EditorState, Prec, RangeSetBuilder, StateEffect, StateField, type Extension } from '@codemirror/state';
import {
  Decoration,
  type DecorationSet,
  drawSelection,
  dropCursor,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  keymap,
  lineNumbers,
  placeholder as placeholderExt,
  rectangularSelection,
  ViewPlugin,
  type ViewUpdate,
} from '@codemirror/view';
import { tags as t } from '@lezer/highlight';
import { useEffect, useRef } from 'react';
import { splitStatements, statementAt } from '../lib/sqlsplit';
import type { CompletionTable, Engine } from '../lib/types';

export type RunMode = 'current' | 'all';

export interface SqlEditorProps {
  value: string;
  engine: Engine;
  onChange?(v: string): void;
  readOnly?: boolean;
  completions?: CompletionTable[];
  defaultSchema?: string;
  fontSize?: number;
  minimal?: boolean;
  autoFocus?: boolean;
  placeholder?: string;
  onRun?(mode: RunMode): void;
  onFormat?(): void;
  onSave?(): void;
  /** Mod-I: open the AI assistant. */
  onAsk?(): void;
  onReady?(view: EditorView): void;
}

const highlight = HighlightStyle.define([
  { tag: t.keyword, color: 'var(--syn-keyword)', fontWeight: '500' },
  { tag: [t.operatorKeyword, t.modifier], color: 'var(--syn-keyword)' },
  { tag: [t.string, t.special(t.string)], color: 'var(--syn-string)' },
  { tag: [t.number, t.bool, t.null], color: 'var(--syn-number)' },
  {
    tag: [t.comment, t.lineComment, t.blockComment],
    color: 'var(--syn-comment)',
    fontStyle: 'italic',
  },
  { tag: [t.typeName, t.standard(t.name)], color: 'var(--syn-type)' },
  {
    tag: [t.function(t.variableName), t.function(t.name)],
    color: 'var(--syn-fn)',
  },
  {
    tag: [t.special(t.name), t.special(t.variableName)],
    color: 'var(--syn-ident)',
  },
  {
    tag: [t.operator, t.punctuation, t.separator, t.bracket],
    color: 'var(--syn-punct)',
  },
  { tag: t.variableName, color: 'var(--text)' },
]);

const baseTheme = EditorView.theme({
  '&': { height: '100%', backgroundColor: 'transparent', color: 'var(--text)' },
  '&.cm-focused': { outline: 'none' },
  '.cm-scroller': { fontFamily: 'var(--font-mono)', lineHeight: '1.55' },
  '.cm-content': { caretColor: 'var(--accent)', padding: '8px 0' },
  '.cm-cursor, .cm-dropCursor': {
    borderLeftColor: 'var(--accent)',
    borderLeftWidth: '2px',
  },
  '.cm-gutters': {
    backgroundColor: 'transparent',
    color: 'var(--text-faint)',
    border: 'none',
    paddingLeft: '4px',
  },
  '.cm-activeLineGutter': {
    backgroundColor: 'transparent',
    color: 'var(--text-dim)',
  },
  '.cm-activeLine': { backgroundColor: 'var(--editor-active-line)' },
  '&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, ::selection': {
    backgroundColor: 'var(--editor-selection) !important',
  },
  '.cm-selectionMatch': { backgroundColor: 'var(--editor-match)' },
  '.cm-matchingBracket': {
    backgroundColor: 'var(--editor-match)',
    outline: '1px solid var(--border-strong)',
  },
  '.cm-tooltip': {
    backgroundColor: 'var(--bg-popover)',
    border: '1px solid var(--border)',
    borderRadius: '8px',
    boxShadow: 'var(--shadow-lg)',
    overflow: 'hidden',
  },
  '.cm-tooltip-autocomplete > ul': {
    fontFamily: 'var(--font-mono)',
    fontSize: '12px',
    maxHeight: '18em',
  },
  '.cm-tooltip-autocomplete > ul > li': {
    padding: '3px 10px 3px 6px !important',
  },
  '.cm-tooltip-autocomplete > ul > li[aria-selected]': {
    backgroundColor: 'var(--accent-soft)',
    color: 'var(--text)',
  },
  '.cm-completionDetail': {
    color: 'var(--text-faint)',
    fontStyle: 'normal',
    marginLeft: '10px',
  },
  '.cm-completionIcon': { opacity: '0.7' },
  '.cm-panels': {
    backgroundColor: 'var(--bg-elev)',
    color: 'var(--text)',
    borderColor: 'var(--border)',
  },
  '.cm-panels input, .cm-panels button': { fontSize: '12px' },
  '.cm-searchMatch': { backgroundColor: 'var(--editor-match)' },
  '.cm-searchMatch-selected': { backgroundColor: 'var(--accent-soft)' },
  '.cm-placeholder': { color: 'var(--text-faint)' },
});

// ---- current statement highlight --------------------------------------------------

function currentStatementPlugin(engine: Engine) {
  const mark = Decoration.line({ class: 'cm-current-stmt' });
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(view: EditorView) {
        this.decorations = this.build(view);
      }
      update(u: ViewUpdate) {
        if (u.docChanged || u.selectionSet || u.focusChanged) this.decorations = this.build(u.view);
      }
      build(view: EditorView): DecorationSet {
        const builder = new RangeSetBuilder<Decoration>();
        const { state } = view;
        if (!view.hasFocus || state.doc.length > 400_000) return builder.finish();
        const sel = state.selection.main;
        if (!sel.empty) return builder.finish();
        const text = state.doc.toString();
        const stmts = splitStatements(text, engine);
        if (stmts.length < 2) return builder.finish();
        const s = statementAt(stmts, sel.head, text);
        if (!s) return builder.finish();
        const first = state.doc.lineAt(s.from).number;
        const last = state.doc.lineAt(s.to).number;
        for (let l = first; l <= last; l++) builder.add(state.doc.line(l).from, state.doc.line(l).from, mark);
        return builder.finish();
      }
    },
    { decorations: (v) => v.decorations },
  );
}

// ---- error underline -------------------------------------------------------------

export const setErrorMark = StateEffect.define<{
  from: number;
  to: number;
} | null>();
const errorMarkField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    deco = deco.map(tr.changes);
    for (const e of tr.effects) {
      if (e.is(setErrorMark)) {
        if (!e.value || e.value.to <= e.value.from) deco = Decoration.none;
        else deco = Decoration.set([Decoration.mark({ class: 'cm-sql-error' }).range(e.value.from, e.value.to)]);
      }
    }
    if (tr.docChanged) deco = Decoration.none;
    return deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});

// ---- completion schema -----------------------------------------------------------

function buildNamespace(tables: CompletionTable[]): SQLNamespace {
  const ns: Record<string, Record<string, SQLNamespace>> = {};
  for (const tbl of tables) {
    const cols: Completion[] = tbl.columns.map(([name, type]) => ({
      label: name,
      type: 'property',
      detail: type,
    }));
    (ns[tbl.schema] ??= {})[tbl.name] = {
      self: {
        label: tbl.name,
        type: tbl.kind === 'table' ? 'class' : 'interface',
        detail: tbl.kind,
      },
      children: cols,
    };
  }
  return ns as SQLNamespace;
}

function languageFor(engine: Engine, completions?: CompletionTable[], defaultSchema?: string): Extension {
  const dialect = engine === 'postgres' || engine === 'duckdb' ? PostgreSQL : engine === 'mysql' ? MySQL : engine === 'mssql' ? MSSQL : SQLite;
  return sql({
    dialect,
    upperCaseKeywords: true,
    schema: completions?.length ? buildNamespace(completions) : undefined,
    defaultSchema: defaultSchema || undefined,
  });
}

export function SqlEditor(props: SqlEditorProps) {
  const host = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const handlers = useRef(props);
  handlers.current = props;
  const lang = useRef(new Compartment());
  const ro = useRef(new Compartment());
  const font = useRef(new Compartment());

  useEffect(() => {
    const fontTheme = (size: number) => EditorView.theme({ '&': { fontSize: `${size}px` } });
    const runKeys = Prec.highest(
      keymap.of([
        {
          key: 'Mod-Enter',
          run: () => {
            handlers.current.onRun?.('current');
            return true;
          },
        },
        {
          key: 'Shift-Mod-Enter',
          run: () => {
            handlers.current.onRun?.('all');
            return true;
          },
        },
        {
          key: 'Shift-Mod-f',
          run: () => {
            handlers.current.onFormat?.();
            return true;
          },
        },
        {
          key: 'Mod-s',
          run: () => {
            handlers.current.onSave?.();
            return true;
          },
        },
        {
          key: 'Mod-i',
          run: () => {
            if (!handlers.current.onAsk) return false;
            handlers.current.onAsk();
            return true;
          },
        },
        { key: 'Mod-/', run: toggleComment },
      ]),
    );
    const extensions: Extension[] = [
      props.minimal ? [] : [lineNumbers(), highlightActiveLineGutter(), highlightActiveLine()],
      history(),
      drawSelection(),
      dropCursor(),
      indentOnInput(),
      bracketMatching(),
      closeBrackets(),
      rectangularSelection(),
      highlightSelectionMatches(),
      search({ top: true }),
      autocompletion({ activateOnTyping: true, icons: true }),
      runKeys,
      keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...searchKeymap, ...historyKeymap, ...completionKeymap, indentWithTab]),
      syntaxHighlighting(highlight),
      baseTheme,
      errorMarkField,
      props.minimal ? [] : currentStatementPlugin(props.engine),
      lang.current.of(languageFor(props.engine, props.completions, props.defaultSchema)),
      ro.current.of([EditorState.readOnly.of(!!props.readOnly), EditorView.editable.of(!props.readOnly)]),
      font.current.of(fontTheme(props.fontSize ?? 13)),
      EditorView.lineWrapping,
      props.placeholder ? placeholderExt(props.placeholder) : [],
      EditorView.updateListener.of((u) => {
        if (u.docChanged) handlers.current.onChange?.(u.state.doc.toString());
      }),
    ];
    const view = new EditorView({
      parent: host.current!,
      state: EditorState.create({ doc: props.value, extensions }),
    });
    viewRef.current = view;
    props.onReady?.(view);
    if (props.autoFocus) setTimeout(() => view.focus(), 30);
    return () => {
      view.destroy();
      viewRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // External value changes (e.g. format, history insert)
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    const cur = view.state.doc.toString();
    if (cur !== props.value) {
      view.dispatch({
        changes: { from: 0, to: cur.length, insert: props.value },
      });
    }
  }, [props.value]);

  useEffect(() => {
    viewRef.current?.dispatch({
      effects: lang.current.reconfigure(languageFor(props.engine, props.completions, props.defaultSchema)),
    });
  }, [props.engine, props.completions, props.defaultSchema]);

  useEffect(() => {
    viewRef.current?.dispatch({
      effects: ro.current.reconfigure([EditorState.readOnly.of(!!props.readOnly), EditorView.editable.of(!props.readOnly)]),
    });
  }, [props.readOnly]);

  useEffect(() => {
    viewRef.current?.dispatch({
      effects: font.current.reconfigure(EditorView.theme({ '&': { fontSize: `${props.fontSize ?? 13}px` } })),
    });
  }, [props.fontSize]);

  return <div ref={host} className="sql-editor" />;
}

export function CodeView({ value, engine, fontSize = 12 }: { value: string; engine: Engine; fontSize?: number }) {
  return <SqlEditor value={value} engine={engine} readOnly minimal fontSize={fontSize} />;
}
