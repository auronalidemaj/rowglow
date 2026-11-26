import { invoke } from '@tauri-apps/api/core';
import type { AppErrorShape } from './types';

export const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
export const isMac =
  typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

const BRIDGE_URL = 'http://127.0.0.1:1425/rpc';

export class RpcError extends Error implements AppErrorShape {
  kind: string;
  code?: string;
  detail?: string;
  hint?: string;
  position?: number;
  constructor(e: AppErrorShape) {
    super(e.message);
    this.kind = e.kind;
    this.code = e.code;
    this.detail = e.detail;
    this.hint = e.hint;
    this.position = e.position;
  }
}

function toRpcError(e: unknown): RpcError {
  if (e instanceof RpcError) return e;
  if (e && typeof e === 'object' && 'message' in e) {
    const o = e as AppErrorShape;
    return new RpcError({ ...o, kind: o.kind ?? 'internal' });
  }
  return new RpcError({ kind: 'internal', message: String(e) });
}

export async function call<T>(method: string, params: Record<string, unknown> = {}): Promise<T> {
  if (isTauri) {
    try {
      return await invoke<T>('rpc', { method, params });
    } catch (e) {
      throw toRpcError(e);
    }
  }
  let res: Response;
  try {
    res = await fetch(BRIDGE_URL, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ method, params }),
    });
  } catch {
    throw new RpcError({
      kind: 'internal',
      message: 'Rowglow core is not running. Start it with `pnpm bridge` (dev) or open the desktop app.',
    });
  }
  const body = await res.json();
  if (body.error) throw toRpcError(body.error);
  return body.ok as T;
}

export function errorMessage(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (e && typeof e === 'object' && 'message' in e) return String((e as { message: unknown }).message);
  return String(e);
}
