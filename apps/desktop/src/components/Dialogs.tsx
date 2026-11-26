import clsx from 'clsx';
import { AlertTriangle, CheckCircle2, Info, Sparkles, X, XCircle } from 'lucide-react';
import { useState } from 'react';
import { createPortal } from 'react-dom';
import { keychainName, mod, openExternal } from '../lib/platform';
import { call, errorMessage } from '../lib/rpc';
import type { Snippet } from '../lib/types';
import { useStore } from '../state/store';
import { CodeView } from './SqlEditor';
import { Button, Field, Kbd, Modal, Spinner, Toggle } from './ui';

export function ConfirmDialog() {
  const c = useStore((s) => s.confirmState);
  const resolve = useStore((s) => s.resolveConfirm);
  const [typed, setTyped] = useState('');
  if (!c) return null;
  const blocked = !!c.requireText && typed !== c.requireText;
  const close = (ok: boolean) => {
    setTyped('');
    resolve(ok);
  };
  return (
    <Modal
      title={
        <span className="confirm-title">
          {c.danger && <AlertTriangle size={16} className="text-danger" />} {c.title}
        </span>
      }
      onClose={() => close(false)}
      width={c.code ? 620 : 440}
      footer={
        <>
          <Button onClick={() => close(false)}>Cancel</Button>
          <Button variant={c.danger ? 'danger' : 'primary'} disabled={blocked} onClick={() => close(true)} autoFocus={!c.requireText}>
            {c.confirmLabel ?? 'OK'}
          </Button>
        </>
      }
    >
      {c.message && <p className="confirm-msg">{c.message}</p>}
      {c.code && (
        <div className="confirm-code">
          <CodeView value={c.code} engine="postgres" />
        </div>
      )}
      {c.requireText && (
        <Field
          label={
            <>
              Type <b className="mono">{c.requireText}</b> to confirm
            </>
          }
        >
          <input
            className="input mono"
            autoFocus
            value={typed}
            onChange={(e) => setTyped(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && !blocked && close(true)}
          />
        </Field>
      )}
    </Modal>
  );
}

export function PasswordPrompt({ connectionId, database, error }: { connectionId: string; database?: string; error?: string }) {
  const conn = useStore((s) => s.connections.find((c) => c.id === connectionId));
  const { connect, openModal } = useStore.getState();
  const [pw, setPw] = useState('');
  const [save, setSave] = useState(true);
  const [busy, setBusy] = useState(false);
  if (!conn) return null;
  const submit = async () => {
    setBusy(true);
    await connect(conn.id, { password: pw, savePassword: save, database });
    setBusy(false);
  };
  return (
    <Modal
      title={`Password for ${conn.name}`}
      onClose={() => openModal(null)}
      width={420}
      footer={
        <>
          <Button onClick={() => openModal(null)}>Cancel</Button>
          <Button variant="primary" onClick={submit} disabled={busy}>
            {busy && <Spinner size={12} />} Connect
          </Button>
        </>
      }
    >
      <p className="dim">
        {conn.username || 'The user'}@{conn.host || 'localhost'} needs a password.
      </p>
      {error && <div className="callout callout-error">{error}</div>}
      <Field label="Password">
        <input
          className="input"
          type="password"
          autoFocus
          value={pw}
          onChange={(e) => setPw(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && submit()}
        />
      </Field>
      <Toggle checked={save} onChange={setSave} label={`Remember in ${keychainName}`} />
    </Modal>
  );
}

export function UpgradeModal({ reason }: { reason: string }) {
  const { openModal } = useStore.getState();
  const apiBase = useStore((s) => s.appInfo?.apiBase ?? '');
  return (
    <Modal title="" onClose={() => openModal(null)} width={460} className="upgrade-modal">
      <div className="upgrade">
        <div className="upgrade-icon">
          <Sparkles size={22} />
        </div>
        <h2>Upgrade to Rowglow Pro</h2>
        <p className="dim">{reason}</p>
        <ul className="upgrade-list">
          <li>Unlimited saved connections</li>
          <li>Unlimited query history & saved queries</li>
          <li>Stream entire tables to CSV, JSON or SQL</li>
          <li>Use it on 2 computers (Mac, Windows or Linux) · 1 year of updates included</li>
        </ul>
        <div className="upgrade-actions">
          <Button variant="primary" disabled={!apiBase} onClick={() => openExternal(`${apiBase}/pricing`)}>
            Buy Rowglow Pro
          </Button>
          <Button onClick={() => openModal({ kind: 'settings', section: 'license' })}>I have a license key</Button>
        </div>
      </div>
    </Modal>
  );
}

const SHORTCUTS: [string, string][] = [
  [`${mod}K`, 'Open anything (tables, queries, commands)'],
  [`${mod}N`, 'New connection'],
  [`${mod}T`, 'New query tab'],
  [`${mod}W`, 'Close tab'],
  [`⇧${mod}] / ⇧${mod}[`, 'Next / previous tab'],
  [`${mod}1 … ${mod}9`, 'Switch connection'],
  [`${mod}0`, 'Connections home'],
  [`${mod}↵`, 'Run statement at cursor'],
  [`⇧${mod}↵`, 'Run all statements'],
  [`${mod}.`, 'Cancel running query'],
  [`⇧${mod}F`, 'Format SQL'],
  [`${mod}I`, 'Ask AI (query tab)'],
  [`${mod}S`, 'Commit staged changes / save query'],
  [`⇧${mod}S`, 'Review changes as SQL'],
  [`⇧${mod}Z`, 'Discard staged changes'],
  [`${mod}R`, 'Reload table'],
  [`${mod}F`, 'Filter rows'],
  [`${mod}I`, 'Insert row (table tab)'],
  [`${mod}D`, 'Duplicate selected rows'],
  [`${mod}⌫`, 'Delete selected rows'],
  [`${mod}[ / ${mod}]`, 'Previous / next page'],
  [`${mod}\\`, 'Toggle sidebar'],
  [`⇧${mod}I`, 'Toggle inspector'],
  [`${mod},`, 'Settings'],
];

export function ShortcutsModal() {
  const { openModal } = useStore.getState();
  return (
    <Modal title="Keyboard shortcuts" onClose={() => openModal(null)} width={520}>
      <div className="shortcuts">
        {SHORTCUTS.map(([k, d]) => (
          <div key={k} className="shortcut-row">
            <span>{d}</span>
            <Kbd>{k}</Kbd>
          </div>
        ))}
      </div>
    </Modal>
  );
}

export function SaveSnippetModal({ sessionId, tabId, sql, name: initialName }: { sessionId: string; tabId: string; sql: string; name?: string }) {
  const { openModal, updateTab, loadSnippets, toast, handleLimitError } = useStore.getState();
  const ws = useStore((s) => s.workspaces.find((w) => w.sessionId === sessionId));
  const [name, setName] = useState(initialName ?? '');
  const [scoped, setScoped] = useState(true);
  const [busy, setBusy] = useState(false);
  const save = async () => {
    setBusy(true);
    try {
      const snip = await call<Snippet>('snippets.save', {
        snippet: {
          id: '',
          name: name.trim(),
          sql,
          connectionId: scoped ? ws?.connection.id : null,
        },
      });
      updateTab(sessionId, tabId, {
        snippetId: snip.id,
        title: snip.name,
        dirty: false,
      });
      await loadSnippets();
      openModal(null);
      toast('success', `Saved “${snip.name}”`, 'Find it under Saved queries in the sidebar.');
    } catch (e) {
      if (!handleLimitError(e)) toast('error', 'Couldn’t save', errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title="Save query"
      onClose={() => openModal(null)}
      width={420}
      footer={
        <>
          <Button onClick={() => openModal(null)}>Cancel</Button>
          <Button variant="primary" onClick={save} disabled={!name.trim() || busy}>
            Save
          </Button>
        </>
      }
    >
      <Field label="Name">
        <input
          className="input"
          autoFocus
          value={name}
          placeholder="Active users this week"
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && name.trim() && save()}
        />
      </Field>
      <Toggle checked={scoped} onChange={setScoped} label={`Only show for ${ws?.connection.name ?? 'this connection'}`} />
    </Modal>
  );
}

export function Toasts() {
  const toasts = useStore((s) => s.toasts);
  const dismiss = useStore((s) => s.dismissToast);
  return createPortal(
    <div className="toasts">
      {toasts.map((t) => (
        <div key={t.id} className={clsx('toast', `toast-${t.kind}`)}>
          <span className="toast-icon">
            {t.kind === 'success' ? <CheckCircle2 size={15} /> : t.kind === 'error' ? <XCircle size={15} /> : <Info size={15} />}
          </span>
          <div className="toast-text">
            <div className="toast-title">{t.title}</div>
            {t.body && <div className="toast-body">{t.body}</div>}
          </div>
          <button className="toast-close" onClick={() => dismiss(t.id)} aria-label="Dismiss">
            <X size={12} />
          </button>
        </div>
      ))}
    </div>,
    document.body,
  );
}

export function Onboarding() {
  const settings = useStore((s) => s.settings);
  const setSettings = useStore((s) => s.setSettings);
  // Usage stats are opt-in (never pre-ticked). Local Radar stays on this computer.
  const [telemetry, setTelemetry] = useState(false);
  const [radar, setRadar] = useState(settings.radarOnLaunch);
  const finish = () => {
    void setSettings({ onboarded: true, telemetry, radarOnLaunch: radar });
    if (radar) void useStore.getState().scanRadar();
  };
  return (
    <Modal onClose={finish} width={480} className="onboarding">
      <div className="upgrade">
        <div className="upgrade-icon">
          <Sparkles size={22} />
        </div>
        <h2>Welcome to Rowglow</h2>
        <p className="dim">A fast, local-first database client. A few things worth knowing:</p>
        <ul className="upgrade-list">
          <li>
            <b>Local Radar</b> looks for Postgres, MySQL, MariaDB, SQL Server, Redis and MongoDB running on this computer (including Docker). It only checks this machine, never
            your network, and logs in only when you click Connect.
          </li>
          <li>
            <b>Edits are staged.</b> Nothing touches your data until you press <Kbd>{mod}S</Kbd>, and you can review the SQL first.
          </li>
          <li>
            <Kbd>{mod}K</Kbd> opens any table, saved query or command.
          </li>
        </ul>
        <div className="onboarding-toggle">
          <Toggle checked={radar} onChange={setRadar} label="Look for databases on this computer when Rowglow opens" />
          <Toggle
            checked={telemetry}
            onChange={setTelemetry}
            label="Share anonymous usage stats (app version & OS only — never hostnames, schemas or SQL)"
          />
        </div>
        <div className="upgrade-actions">
          <Button variant="primary" onClick={finish}>
            Get started
          </Button>
        </div>
      </div>
    </Modal>
  );
}
