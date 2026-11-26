import clsx from 'clsx';
import { CheckCircle2, ExternalLink, KeyRound, RefreshCw, Sparkles } from 'lucide-react';
import { useEffect, useState } from 'react';
import { formatDate, formatDayUtc, relativeTime } from '../lib/format';
import { copyText, mod, openExternal } from '../lib/platform';
import { call, errorMessage } from '../lib/rpc';
import type { AiInfo, AiModel, AiSettings, LicenseStatus } from '../lib/types';
import { useStore } from '../state/store';
import { Button, Field, LogoMark, Modal, Segmented, Spinner, Toggle } from './ui';

type Section = 'general' | 'editor' | 'ai' | 'license' | 'privacy' | 'advanced' | 'about';

const SECTIONS: { id: Section; label: string }[] = [
  { id: 'general', label: 'General' },
  { id: 'editor', label: 'Editor & data' },
  { id: 'ai', label: 'AI assistant' },
  { id: 'license', label: 'License' },
  { id: 'privacy', label: 'Privacy' },
  { id: 'advanced', label: 'Advanced' },
  { id: 'about', label: 'About' },
];

export function SettingsModal({ section: initial }: { section?: Section }) {
  const [section, setSection] = useState<Section>(initial ?? 'general');
  const { openModal } = useStore.getState();
  return (
    <Modal title="Settings" onClose={() => openModal(null)} width={720} className="settings-modal">
      <div className="settings">
        <nav className="settings-nav">
          {SECTIONS.map((s) => (
            <button key={s.id} className={clsx('settings-nav-item', section === s.id && 'is-active')} onClick={() => setSection(s.id)}>
              {s.label}
            </button>
          ))}
        </nav>
        <div className="settings-body">
          {section === 'general' && <General />}
          {section === 'editor' && <EditorSettings />}
          {section === 'ai' && <AiPane />}
          {section === 'license' && <LicensePane />}
          {section === 'privacy' && <Privacy />}
          {section === 'advanced' && <Advanced />}
          {section === 'about' && <About />}
        </div>
      </div>
    </Modal>
  );
}

function General() {
  const s = useStore((st) => st.settings);
  const set = useStore((st) => st.setSettings);
  return (
    <div className="settings-group">
      <Field label="Appearance">
        <Segmented
          value={s.theme}
          onChange={(theme) => set({ theme })}
          options={[
            { value: 'system', label: 'System' },
            { value: 'light', label: 'Light' },
            { value: 'dark', label: 'Dark' },
          ]}
        />
      </Field>
      <Field label="Density">
        <Segmented
          value={s.density}
          onChange={(density) => set({ density })}
          options={[
            { value: 'compact', label: 'Compact' },
            { value: 'comfortable', label: 'Comfortable' },
          ]}
        />
      </Field>
      <Toggle
        checked={s.confirmProdWrites}
        onChange={(v) => set({ confirmProdWrites: v })}
        label="Ask before running writes on production connections"
      />
      <Toggle
        checked={s.confirmDangerous}
        onChange={(v) => set({ confirmDangerous: v })}
        label="Ask before DELETE/UPDATE without WHERE, DROP and TRUNCATE (every connection)"
      />
      <Toggle
        checked={s.showSystemSchemas}
        onChange={(v) => set({ showSystemSchemas: v })}
        label="Show system schemas (pg_catalog, information_schema, mysql…)"
      />
    </div>
  );
}

function EditorSettings() {
  const s = useStore((st) => st.settings);
  const set = useStore((st) => st.setSettings);
  return (
    <div className="settings-group">
      <Field label="Editor font size">
        <div className="range-row">
          <input type="range" min={11} max={20} value={s.editorFontSize} onChange={(e) => set({ editorFontSize: Number(e.target.value) })} />
          <span className="mono">{s.editorFontSize}px</span>
        </div>
      </Field>
      <Field label="Query result limit" hint="Rows kept per result in the editor. Larger results are truncated with a notice.">
        <select className="select" value={s.resultLimit} onChange={(e) => set({ resultLimit: Number(e.target.value) })}>
          {[500, 2000, 10000, 50000, 200000].map((n) => (
            <option key={n} value={n}>
              {n.toLocaleString()} rows
            </option>
          ))}
        </select>
      </Field>
      <Field label="Default page size for tables">
        <select className="select" value={s.pageSize} onChange={(e) => set({ pageSize: Number(e.target.value) })}>
          {[100, 300, 1000, 5000].map((n) => (
            <option key={n} value={n}>
              {n.toLocaleString()} rows
            </option>
          ))}
        </select>
      </Field>
    </div>
  );
}

function LicensePane() {
  const license = useStore((s) => s.license);
  const apiBase = useStore((s) => s.appInfo?.apiBase ?? '');
  const { toast } = useStore.getState();
  const [key, setKey] = useState('');
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const run = async (what: string, fn: () => Promise<LicenseStatus>) => {
    setBusy(what);
    setError(null);
    try {
      const st = await fn();
      useStore.setState({ license: st });
      if (st.message) toast('info', st.message);
      return st;
    } catch (e) {
      setError(errorMessage(e));
      return null;
    } finally {
      setBusy(null);
    }
  };

  if (!license) return <Spinner />;

  if (license.licensed) {
    return (
      <div className="settings-group">
        <div className="license-card is-pro">
          <div className="license-badge">
            <Sparkles size={16} /> Rowglow {license.plan === 'team' ? 'Team' : 'Pro'}
          </div>
          <div className="license-rows">
            <div>
              <span className="dim">Licensed to</span> {license.name || license.email}
            </div>
            <div>
              <span className="dim">Email</span> {license.email}
            </div>
            <div>
              <span className="dim">Key</span> <span className="mono">{license.keyHint}</span>
            </div>
            {license.updatesUntil ? (
              <div>
                <span className="dim">Updates until</span> {formatDate(license.updatesUntil)}
              </div>
            ) : null}
            {license.lastValidated ? (
              <div>
                <span className="dim">Last checked</span> {relativeTime(license.lastValidated)}
              </div>
            ) : null}
          </div>
        </div>
        <div className="row-actions">
          <Button onClick={() => run('refresh', () => call('license.refresh'))} disabled={!!busy}>
            {busy === 'refresh' ? <Spinner size={12} /> : <RefreshCw size={13} />} Refresh
          </Button>
          <Button
            variant="danger"
            onClick={() =>
              run('deactivate', () => call('license.deactivate')).then(
                (s) => s && toast('success', 'This computer was deactivated', 'Your seat is free to use on another device.'),
              )
            }
            disabled={!!busy}
          >
            {busy === 'deactivate' && <Spinner size={12} />} Deactivate this computer
          </Button>
        </div>
        {error && <div className="callout callout-error">{error}</div>}
      </div>
    );
  }

  return (
    <div className="settings-group">
      {license.freeUntil ? (
        <div className="license-card is-pro">
          <div className="license-badge">
            <Sparkles size={16} /> Every feature is free
          </div>
          <p className="dim">
            Rowglow is free for everyone until {formatDayUtc(license.freeUntil)}: unlimited connections, history, saved queries, full table
            exports, CSV import and the AI assistant. No license key or account needed.
          </p>
        </div>
      ) : (
        <div className="license-card">
          <div className="license-badge">Free plan</div>
          <p className="dim">
            Includes every engine, the editor, staged edits and Local Radar — with {license.limits.connections} saved connections,{' '}
            {license.limits.history} history entries and {license.limits.snippets} saved queries.
          </p>
          {license.message && <div className="callout callout-warn">{license.message}</div>}
          <Button variant="primary" disabled={!apiBase} onClick={() => openExternal(`${apiBase}/pricing`)}>
            <Sparkles size={13} /> Get Rowglow Pro <ExternalLink size={12} />
          </Button>
        </div>
      )}
      <Field label={license.freeUntil ? 'Have a license key? Enter it here' : 'Already purchased? Enter your license key'}>
        <div className="file-row">
          <input
            className="input mono"
            placeholder="RGLW-XXXXX-XXXXX-XXXXX-XXXXX"
            value={key}
            onChange={(e) => setKey(e.target.value.toUpperCase())}
            onKeyDown={(e) => e.key === 'Enter' && key && run('activate', () => call('license.activate', { key }))}
            spellCheck={false}
          />
          <Button
            variant="primary"
            disabled={!key || !!busy}
            onClick={() =>
              run('activate', () => call('license.activate', { key })).then(
                (s) => s?.licensed && toast('success', 'Rowglow Pro activated', 'Thank you for supporting Rowglow!'),
              )
            }
          >
            {busy === 'activate' ? <Spinner size={12} /> : <KeyRound size={13} />} Activate
          </Button>
        </div>
      </Field>
      {error && <div className="callout callout-error">{error}</div>}
      <p className="dim small">
        Device ID <span className="mono">{license.deviceId}</span>
      </p>
    </div>
  );
}

/** The model chosen for a provider. */
function modelOf(s: AiSettings, provider: string): string {
  if (provider === 'anthropic') return s.model;
  if (provider === 'ollama') return s.ollamaModel;
  return s.models?.[provider] ?? '';
}

function withModel(s: AiSettings, provider: string, model: string): AiSettings {
  if (provider === 'anthropic') return { ...s, model };
  if (provider === 'ollama') return { ...s, ollamaModel: model };
  return { ...s, models: { ...(s.models ?? {}), [provider]: model } };
}

function AiPane() {
  const toast = useStore((st) => st.toast);
  const [info, setInfo] = useState<AiInfo | null>(null);
  const [draft, setDraft] = useState<AiSettings | null>(null);
  const [key, setKey] = useState('');
  const [models, setModels] = useState<AiModel[] | null>(null);
  const [modelsError, setModelsError] = useState<string | null>(null);
  const [typeModel, setTypeModel] = useState(false);
  const [busy, setBusy] = useState<'save' | 'test' | null>(null);
  const [result, setResult] = useState<{ ok: boolean; message: string } | null>(null);

  const load = () =>
    call<AiInfo>('ai.settings')
      .then((i) => {
        setInfo(i);
        setDraft(i.settings);
      })
      .catch((e) => toast('error', 'Couldn’t load AI settings', errorMessage(e)));
  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const provider = info?.providers.find((p) => p.id === draft?.provider);
  const hasSavedKey = !!(provider && info?.keys[provider.id]);
  const canList = !!provider && (!provider.needsKey || hasSavedKey || key.trim().length > 8);
  const listKey = `${draft?.provider}|${draft?.ollamaUrl}|${draft?.customUrl}|${key.trim()}|${hasSavedKey}`;

  // Load the provider's models (free API calls), once the key is known.
  useEffect(() => {
    if (!draft || !provider) return;
    setModels(null);
    setModelsError(null);
    if (!canList) return;
    let stale = false;
    const t = setTimeout(() => {
      call<{ models: AiModel[]; recommended: string | null }>('ai.models', { settings: draft, apiKey: key.trim() || null })
        .then((r) => {
          if (stale) return;
          setModels(r.models);
          const current = modelOf(draft, provider.id);
          if (!current && r.recommended) setDraft((d) => (d ? withModel(d, provider.id, r.recommended!) : d));
          setTypeModel(!!current && r.models.length > 0 && !r.models.some((m) => m.id === current) ? true : r.models.length === 0);
        })
        .catch((e) => !stale && setModelsError(errorMessage(e)));
    }, 400);
    return () => {
      stale = true;
      clearTimeout(t);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [listKey]);

  if (!info || !draft || !provider) return <Spinner />;
  const patch = (p: Partial<AiSettings>) => {
    setDraft({ ...draft, ...p });
    setResult(null);
  };
  const model = modelOf(draft, provider.id);

  const save = async () => {
    setBusy('save');
    try {
      await call('ai.saveSettings', { settings: draft, apiKey: key || null });
      setKey('');
      await load();
      toast('success', 'AI settings saved');
    } catch (e) {
      setResult({ ok: false, message: errorMessage(e) });
    } finally {
      setBusy(null);
    }
  };
  const test = async () => {
    setBusy('test');
    setResult(null);
    try {
      const r = await call<{ model: string }>('ai.test', { settings: draft, apiKey: key || null });
      setResult({ ok: true, message: `Ready · ${r.model}` });
    } catch (e) {
      setResult({ ok: false, message: errorMessage(e) });
    } finally {
      setBusy(null);
    }
  };
  const keysHost = provider.keysUrl ? provider.keysUrl.replace(/^https?:\/\//, '').split('/')[0] : '';

  return (
    <div className="settings-group">
      <p className="dim small">
        Ask for a query in plain English, get a query explained, or have a failed one fixed. Press {mod}I in a query tab. Rowglow inserts the answer
        into your editor; it never runs anything on its own.
      </p>
      <Field
        label="Provider"
        hint={provider.local ? 'Runs on your computer, so your schema never leaves it.' : 'Use your own account; the provider bills you directly.'}
      >
        <select
          className="select"
          value={draft.provider}
          onChange={(e) => {
            patch({ provider: e.target.value });
            setKey('');
            setTypeModel(false);
          }}
        >
          <optgroup label="With your API key">
            {info.providers
              .filter((p) => p.needsKey)
              .map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
          </optgroup>
          <optgroup label="Other">
            {info.providers
              .filter((p) => !p.needsKey)
              .map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
          </optgroup>
        </select>
      </Field>

      {provider.id === 'ollama' && (
        <Field label="Ollama address">
          <input
            className="input mono"
            value={draft.ollamaUrl}
            placeholder="http://127.0.0.1:11434"
            onChange={(e) => patch({ ollamaUrl: e.target.value })}
            spellCheck={false}
          />
        </Field>
      )}
      {provider.id === 'custom' && (
        <Field label="API address" hint="Any server that speaks the OpenAI Chat Completions API, e.g. LM Studio, vLLM, Together or Azure OpenAI.">
          <input
            className="input mono"
            value={draft.customUrl ?? ''}
            placeholder="https://example.com/v1"
            onChange={(e) => patch({ customUrl: e.target.value })}
            spellCheck={false}
          />
        </Field>
      )}

      {(provider.needsKey || provider.id === 'custom') && (
        <Field
          label={provider.needsKey ? `${provider.name.replace(/ \(.*\)$/, '')} API key` : 'API key (if the server needs one)'}
          hint={
            hasSavedKey && !key ? (
              <>
                Saved in your system keychain ·{' '}
                <button
                  type="button"
                  className="link-btn"
                  onClick={async () => {
                    await call('ai.saveSettings', { settings: draft, clearKey: true });
                    await load();
                  }}
                >
                  remove
                </button>
              </>
            ) : keysHost ? (
              <>
                Create one at{' '}
                <button type="button" className="link-btn" onClick={() => openExternal(provider.keysUrl)}>
                  {keysHost}
                </button>
                . Usage is billed to your account.
              </>
            ) : undefined
          }
        >
          <input
            className="input mono"
            type="password"
            value={key}
            placeholder={hasSavedKey ? '••••••••' : provider.id === 'anthropic' ? 'sk-ant-…' : 'Paste your key'}
            onChange={(e) => {
              setKey(e.target.value);
              setResult(null);
            }}
            autoComplete="off"
            spellCheck={false}
          />
        </Field>
      )}

      <Field
        label="Model"
        hint={
          modelsError ? (
            <span className="text-danger">{modelsError}</span>
          ) : !canList ? (
            'Enter your key to load the models it can use.'
          ) : models && !models.length && provider.id === 'ollama' ? (
            'No models yet. In a terminal, run: ollama pull qwen2.5-coder:7b'
          ) : provider.id === 'anthropic' ? (
            'Haiku is the fastest and costs least; Opus is the most capable.'
          ) : models ? (
            <>
              {models.length} models available ·{' '}
              <button type="button" className="link-btn" onClick={() => setTypeModel((v) => !v)}>
                {typeModel ? 'pick from the list' : 'type a model ID'}
              </button>
            </>
          ) : (
            'Loading the models…'
          )
        }
      >
        {models && models.length > 0 && !typeModel ? (
          <select className="select" value={model} onChange={(e) => patch(withModel(draft, provider.id, e.target.value))}>
            {!model && <option value="">Choose a model</option>}
            {model && !models.some((m) => m.id === model) && <option value={model}>{model} (not in the list)</option>}
            {models.map((m) => (
              <option key={m.id} value={m.id}>
                {m.name !== m.id ? `${m.name} — ${m.id}` : m.id}
                {m.note ? ` · ${m.note}` : ''}
              </option>
            ))}
          </select>
        ) : (
          <input
            className="input mono"
            value={model}
            placeholder={provider.id === 'ollama' ? 'qwen2.5-coder:7b' : 'model ID'}
            onChange={(e) => patch(withModel(draft, provider.id, e.target.value.trim()))}
            spellCheck={false}
          />
        )}
      </Field>

      <div className="file-row">
        <Button variant="primary" onClick={save} disabled={!!busy}>
          {busy === 'save' ? <Spinner size={12} /> : null} Save
        </Button>
        <Button onClick={test} disabled={!!busy}>
          {busy === 'test' ? <Spinner size={12} /> : null} Test
        </Button>
        {info.perDay != null && (
          <span className="dim small">
            Free plan: {Math.max(0, info.perDay - info.usedToday)} of {info.perDay} requests left today
          </span>
        )}
      </div>
      {result && (
        <div className={clsx('callout', result.ok ? 'callout-ok' : 'callout-error')}>
          {result.ok ? <CheckCircle2 size={14} /> : null}
          <span>{result.message}</span>
        </div>
      )}
      <p className="dim small">
        What is sent: your request, the current schema’s table and column names, types and keys, and — to explain or fix a query — that SQL and the
        database’s error message. Never rows or values from your tables.{' '}
        {provider.local
          ? 'It goes to the Ollama server above.'
          : provider.id === 'custom'
            ? 'It goes to the server above.'
            : `It goes straight from this computer to ${provider.name}.`}
      </p>
    </div>
  );
}

function Privacy() {
  const s = useStore((st) => st.settings);
  const set = useStore((st) => st.setSettings);
  return (
    <div className="settings-group">
      <Toggle checked={s.telemetry} onChange={(v) => set({ telemetry: v })} label="Share anonymous usage statistics" />
      <Toggle
        checked={s.radarOnLaunch}
        onChange={(v) => set({ radarOnLaunch: v })}
        label="Look for databases on this computer when Rowglow opens (Local Radar)"
      />
      <p className="dim small">
        Local Radar only checks this computer (127.0.0.1) for database ports and reads your local Docker containers. It never scans your network,
        never sends what it finds anywhere, and only logs in when you click Connect.
      </p>
      <p className="dim small">
        When on, Rowglow sends a launch ping with a random install ID, app version, OS version, CPU architecture and whether a license is active. It
        never sends hostnames, database names, schemas, table names, queries or results.
      </p>
      <p className="dim small">Passwords are stored in your system keychain. Query history and saved queries stay on this computer.</p>
      <p className="dim small">
        The AI assistant is off until you set it up. It then sends your request and table and column names (never row data) to the provider you chose:
        Anthropic, or Ollama on your own computer.
      </p>
    </div>
  );
}

function Advanced() {
  const s = useStore((st) => st.settings);
  const appInfo = useStore((st) => st.appInfo);
  const set = useStore((st) => st.setSettings);
  const [api, setApi] = useState(s.apiBase ?? '');
  const toast = useStore((st) => st.toast);
  return (
    <div className="settings-group">
      {!appInfo?.debug ? (
        <Field label="License & update server" hint={appInfo?.apiBase ? undefined : 'This build has no server configured, so licensing and update checks are off.'}>
          <input className="input mono" readOnly value={appInfo?.apiBase || 'None'} />
        </Field>
      ) : (
      <Field
        label="License & update server"
        hint={
          appInfo?.apiBase
            ? `Development build: currently ${appInfo.apiBase}. Leave empty to use the built-in default.`
            : 'This build has no server configured: licensing and update checks are off until you set one.'
        }
      >
        <div className="file-row">
          <input className="input mono" value={api} placeholder="https://your-domain.com" onChange={(e) => setApi(e.target.value)} />
          <Button
            onClick={async () => {
              await set({ apiBase: (api.trim() || null) as unknown as string });
              const info = await call<typeof appInfo>('app.info');
              useStore.setState({ appInfo: info });
              toast('success', 'Saved');
            }}
          >
            Save
          </Button>
        </div>
      </Field>
      )}
      {appInfo?.secretsInFile && (
        <div className="callout callout-warn">
          Some passwords are stored in Rowglow’s own database (app.db) because this computer’s keychain wasn’t available. The folder is readable
          only by your user account, but the passwords aren’t encrypted. On Linux, installing and unlocking GNOME Keyring or KWallet fixes this
          for passwords you save from then on.
        </div>
      )}
      <Field
        label="Data folder"
        hint="app.db holds saved connections, history and saved queries; passwords go to the system keychain. rowglow.log records crashes."
      >
        <div className="file-row">
          <input className="input mono" readOnly value={appInfo?.dataDir ?? ''} />
          <Button onClick={() => appInfo && copyText(appInfo.dataDir).then(() => toast('success', 'Path copied'))}>Copy</Button>
        </div>
      </Field>
    </div>
  );
}

function About() {
  const appInfo = useStore((s) => s.appInfo);
  const update = useStore((s) => s.update);
  const checkUpdates = useStore((s) => s.checkUpdates);
  const [checking, setChecking] = useState(false);
  return (
    <div className="settings-group about">
      <div className="about-head">
        <LogoMark size={44} />
        <div>
          <div className="about-name">Rowglow</div>
          <div className="dim">
            Version {appInfo?.version} · {appInfo?.platform} {appInfo?.arch}
            {appInfo?.debug ? ' · development build' : ''}
          </div>
        </div>
      </div>
      {update?.updateAvailable ? (
        <div className="callout callout-accent">
          <div>
            <b>Rowglow {update.version}</b> is available
            {update.critical ? ' — this is an important update.' : '.'}
            {update.notes && <div className="update-notes">{update.notes}</div>}
          </div>
          {update.url && (
            <Button variant="primary" onClick={() => openExternal(update.url!)}>
              Download
            </Button>
          )}
        </div>
      ) : update ? (
        <div className="callout callout-ok">
          <CheckCircle2 size={14} /> You’re on the latest version.
        </div>
      ) : null}
      <div className="row-actions">
        <Button
          onClick={async () => {
            setChecking(true);
            await checkUpdates(true);
            setChecking(false);
          }}
          disabled={checking}
        >
          {checking ? <Spinner size={12} /> : <RefreshCw size={13} />} Check for updates
        </Button>
        <Button variant="ghost" disabled={!appInfo?.apiBase} onClick={() => appInfo?.apiBase && openExternal(appInfo.apiBase)}>
          Website <ExternalLink size={12} />
        </Button>
      </div>
      <p className="dim small">© 2026 Rowglow. Built with Rust, Tauri and CodeMirror.</p>
    </div>
  );
}
