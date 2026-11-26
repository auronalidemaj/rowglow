import { create } from 'zustand';
import type { Cell, Column, ColumnDef, ForeignKeyDef } from '../lib/types';

/** What the right-hand inspector shows: the focused row of the active grid. */
export interface InspectTarget {
  owner: string; // tab id
  title: string;
  columns: Column[];
  values: Cell[];
  colDefs?: ColumnDef[];
  editable: boolean;
  fks?: Map<string, ForeignKeyDef>;
  onChange?(col: number, value: Cell): void;
  onFollow?(fk: ForeignKeyDef): void;
}

interface InspectState {
  target: InspectTarget | null;
  set(t: InspectTarget | null): void;
  clear(owner: string): void;
}

export const useInspect = create<InspectState>((set, get) => ({
  target: null,
  set: (target) => set({ target }),
  clear: (owner) => {
    if (get().target?.owner === owner) set({ target: null });
  },
}));

// State modules hold live sessions; a hot swap would orphan them, so reload the page instead.
if (import.meta.hot) import.meta.hot.accept(() => window.location.reload());
