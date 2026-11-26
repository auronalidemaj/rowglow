import { isTauri } from './rpc';

export interface FileFilter {
  name: string;
  extensions: string[];
}

export async function pickOpenFile(title: string, filters?: FileFilter[]): Promise<string | null> {
  if (isTauri) {
    const { open } = await import('@tauri-apps/plugin-dialog');
    const r = await open({ title, multiple: false, directory: false, filters });
    return typeof r === 'string' ? r : null;
  }
  const p = window.prompt(`${title}\n(dev mode: type an absolute path)`);
  return p?.trim() || null;
}

export async function pickSaveFile(title: string, defaultPath: string, filters?: FileFilter[]): Promise<string | null> {
  if (isTauri) {
    const { save } = await import('@tauri-apps/plugin-dialog');
    const r = await save({ title, defaultPath, filters });
    return r ?? null;
  }
  const p = window.prompt(`${title}\n(dev mode: type an absolute path)`, `/tmp/${defaultPath}`);
  return p?.trim() || null;
}

export async function openExternal(url: string): Promise<void> {
  if (isTauri) {
    const { openUrl } = await import('@tauri-apps/plugin-opener');
    await openUrl(url);
  } else {
    window.open(url, '_blank', 'noopener');
  }
}

export async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    document.execCommand('copy');
    ta.remove();
  }
}

export async function onMenu(handler: (id: string) => void): Promise<() => void> {
  if (!isTauri) return () => {};
  const { listen } = await import('@tauri-apps/api/event');
  return listen<string>('menu', (e) => handler(e.payload));
}

export const mod = typeof navigator !== 'undefined' && /Mac/.test(navigator.platform) ? '⌘' : 'Ctrl+';

/** What this OS calls its password store, for UI copy ("Saved in …"). */
export const keychainName =
  typeof navigator === 'undefined' || /Mac/.test(navigator.platform)
    ? 'Keychain'
    : /Win/.test(navigator.platform)
      ? 'Credential Manager'
      : 'the system keyring';
