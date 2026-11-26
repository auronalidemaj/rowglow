import { useEffect, useRef } from 'react';
import { useStore } from '../state/store';

const isMacPlatform = typeof navigator !== 'undefined' && /Mac/.test(navigator.platform);

/** combo examples: "mod+s", "mod+shift+z", "mod+alt+1", "f5" */
export function matches(e: KeyboardEvent, combo: string): boolean {
  const parts = combo.toLowerCase().split('+');
  const key = parts[parts.length - 1];
  const wantMod = parts.includes('mod');
  const wantShift = parts.includes('shift');
  const wantAlt = parts.includes('alt');
  const modPressed = isMacPlatform ? e.metaKey : e.ctrlKey;
  if (wantMod !== modPressed) return false;
  if (wantShift !== e.shiftKey) return false;
  if (wantAlt !== e.altKey) return false;
  if (!isMacPlatform && e.metaKey) return false;
  const k = e.key.toLowerCase();
  if (k === key) return true;
  // e.code fallback for shifted / alt-modified digits and letters
  if (/^[0-9]$/.test(key) && e.code === `Digit${key}`) return true;
  if (/^[a-z]$/.test(key) && e.code === `Key${key.toUpperCase()}`) return true;
  if (key === 'backspace' && k === 'delete') return false;
  return false;
}

export function overlayOpen(): boolean {
  const s = useStore.getState();
  return !!(s.modal || s.confirmState || s.paletteOpen);
}

/** Registers window-level shortcuts while `enabled` (e.g. while a tab is active). */
export function useHotkeys(enabled: boolean, map: Record<string, (e: KeyboardEvent) => void>) {
  const ref = useRef(map);
  ref.current = map;
  useEffect(() => {
    if (!enabled) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || overlayOpen()) return;
      for (const [combo, fn] of Object.entries(ref.current)) {
        if (matches(e, combo)) {
          e.preventDefault();
          e.stopPropagation();
          fn(e);
          return;
        }
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [enabled]);
}
