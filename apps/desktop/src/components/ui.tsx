import clsx from 'clsx';
import { X } from 'lucide-react';
import {
  forwardRef,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type ReactNode,
} from 'react';
import { createPortal } from 'react-dom';
import type { Engine } from '../lib/types';

export const Button = forwardRef<
  HTMLButtonElement,
  ButtonHTMLAttributes<HTMLButtonElement> & { variant?: 'primary' | 'ghost' | 'danger' | 'default'; size?: 'sm' | 'md' }
>(function Button({ variant = 'default', size = 'md', className, ...rest }, ref) {
  return <button ref={ref} type="button" className={clsx('btn', `btn-${variant}`, `btn-${size}`, className)} {...rest} />;
});

export const IconButton = forwardRef<
  HTMLButtonElement,
  ButtonHTMLAttributes<HTMLButtonElement> & { active?: boolean; label: string }
>(function IconButton({ active, label, className, children, ...rest }, ref) {
  return (
    <button
      ref={ref}
      type="button"
      aria-label={label}
      title={label}
      className={clsx('icon-btn', active && 'is-active', className)}
      {...rest}
    >
      {children}
    </button>
  );
});

export function Kbd({ children }: { children: ReactNode }) {
  return <kbd className="kbd">{children}</kbd>;
}

export function Spinner({ size = 14 }: { size?: number }) {
  return <span className="spinner" style={{ width: size, height: size }} aria-label="Loading" />;
}

const ENV_LABEL: Record<string, string> = {
  local: 'Local',
  development: 'Dev',
  staging: 'Staging',
  production: 'Production',
};

export function EnvBadge({ env, compact }: { env: string; compact?: boolean }) {
  if (!env) return null;
  return <span className={clsx('env-badge', `env-${env}`, compact && 'is-compact')}>{ENV_LABEL[env] ?? env}</span>;
}

const ENGINE_META: Record<Engine | 'mariadb', { label: string; short: string }> = {
  postgres: { label: 'PostgreSQL', short: 'Pg' },
  mysql: { label: 'MySQL', short: 'My' },
  mariadb: { label: 'MariaDB', short: 'Ma' },
  sqlite: { label: 'SQLite', short: 'Lt' },
  mssql: { label: 'SQL Server', short: 'Ms' },
  duckdb: { label: 'DuckDB', short: 'Dk' },
  redis: { label: 'Redis', short: 'Rd' },
  mongodb: { label: 'MongoDB', short: 'Mg' },
};

export function EngineIcon({ engine, flavor, size = 28 }: { engine: Engine; flavor?: string; size?: number }) {
  const key = flavor?.toLowerCase() === 'mariadb' ? 'mariadb' : engine;
  const meta = ENGINE_META[key];
  return (
    <span
      className={clsx('engine-icon', `engine-${key}`)}
      style={{ width: size, height: size, fontSize: Math.round(size * 0.4) }}
      title={meta.label}
    >
      {meta.short}
    </span>
  );
}

export function Segmented<T extends string>({
  value,
  options,
  onChange,
  size = 'md',
}: {
  value: T;
  options: { value: T; label: ReactNode; title?: string }[];
  onChange: (v: T) => void;
  size?: 'sm' | 'md';
}) {
  return (
    <div className={clsx('segmented', `segmented-${size}`)} role="tablist">
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          role="tab"
          aria-selected={o.value === value}
          title={o.title}
          className={clsx('segmented-item', o.value === value && 'is-active')}
          onClick={() => onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

export function Field({ label, hint, children, className }: { label: ReactNode; hint?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <label className={clsx('field', className)}>
      <span className="field-label">{label}</span>
      {children}
      {hint && <span className="field-hint">{hint}</span>}
    </label>
  );
}

export function Toggle({ checked, onChange, label }: { checked: boolean; onChange: (v: boolean) => void; label?: ReactNode }) {
  return (
    <label className="toggle">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <span className="toggle-track">
        <span className="toggle-thumb" />
      </span>
      {label && <span className="toggle-label">{label}</span>}
    </label>
  );
}

export function Modal({
  title,
  onClose,
  children,
  footer,
  width = 520,
  className,
}: {
  title?: ReactNode;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
  width?: number;
  className?: string;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [onClose]);
  return createPortal(
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className={clsx('modal', className)} style={{ width }} role="dialog" aria-modal="true">
        {title !== undefined && (
          <div className="modal-header">
            <div className="modal-title">{title}</div>
            <IconButton label="Close" onClick={onClose}>
              <X size={15} />
            </IconButton>
          </div>
        )}
        <div className="modal-body">{children}</div>
        {footer && <div className="modal-footer">{footer}</div>}
      </div>
    </div>,
    document.body,
  );
}

export interface MenuItem {
  label?: ReactNode;
  icon?: ReactNode;
  shortcut?: string;
  onSelect?: () => void;
  danger?: boolean;
  disabled?: boolean;
  separator?: boolean;
}

export function Menu({ x, y, items, onClose }: { x: number; y: number; items: MenuItem[]; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ x, y });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPos({
      x: Math.min(x, window.innerWidth - r.width - 6),
      y: y + r.height > window.innerHeight - 6 ? Math.max(6, y - r.height) : y,
    });
  }, [x, y]);
  useEffect(() => {
    const close = (e: Event) => {
      if (ref.current && e.target instanceof Node && ref.current.contains(e.target)) return;
      onClose();
    };
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose();
    window.addEventListener('mousedown', close, true);
    window.addEventListener('blur', onClose);
    window.addEventListener('keydown', onKey);
    window.addEventListener('resize', onClose);
    return () => {
      window.removeEventListener('mousedown', close, true);
      window.removeEventListener('blur', onClose);
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('resize', onClose);
    };
  }, [onClose]);
  return createPortal(
    <div ref={ref} className="menu" style={{ left: pos.x, top: pos.y }} role="menu" onContextMenu={(e) => e.preventDefault()}>
      {items.map((it, i) =>
        it.separator ? (
          <div key={i} className="menu-sep" />
        ) : (
          <button
            key={i}
            type="button"
            role="menuitem"
            className={clsx('menu-item', it.danger && 'is-danger')}
            disabled={it.disabled}
            onClick={() => {
              onClose();
              it.onSelect?.();
            }}
          >
            <span className="menu-icon">{it.icon}</span>
            <span className="menu-label">{it.label}</span>
            {it.shortcut && <span className="menu-shortcut">{it.shortcut}</span>}
          </button>
        ),
      )}
    </div>,
    document.body,
  );
}

/** Hook: `const [menu, openMenu] = useMenu(); <div onContextMenu={e => openMenu(e, items)}/> {menu}` */
export function useMenu(): [ReactNode, (e: { clientX: number; clientY: number; preventDefault?: () => void }, items: MenuItem[]) => void] {
  const [state, setState] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  const open = useCallback((e: { clientX: number; clientY: number; preventDefault?: () => void }, items: MenuItem[]) => {
    e.preventDefault?.();
    setState({ x: e.clientX, y: e.clientY, items });
  }, []);
  const close = useCallback(() => setState(null), []);
  const node = state ? <Menu x={state.x} y={state.y} items={state.items} onClose={close} /> : null;
  return [node, open];
}

export function EmptyState({ icon, title, children }: { icon?: ReactNode; title: ReactNode; children?: ReactNode }) {
  return (
    <div className="empty-state">
      {icon && <div className="empty-icon">{icon}</div>}
      <div className="empty-title">{title}</div>
      {children && <div className="empty-body">{children}</div>}
    </div>
  );
}

export function LogoMark({ size = 22 }: { size?: number }) {
  // Rowglow "row lantern": a lantern whose glass is table rows, the middle row lit.
  return (
    <svg width={size} height={size} viewBox="0 0 64 64" fill="none" aria-hidden="true">
      <path d="M26 14.5a6 6 0 0 1 12 0" stroke="currentColor" strokeOpacity="0.6" strokeWidth="3.2" strokeLinecap="round" />
      <rect x="21.5" y="16.5" width="21" height="5.5" rx="2.75" fill="currentColor" fillOpacity="0.6" />
      <rect x="17.5" y="24" width="29" height="25" rx="6.5" fill="#F2A93B" fillOpacity="0.14" stroke="#F2A93B" strokeWidth="3.4" />
      <rect x="23.5" y="29.75" width="17" height="3" rx="1.5" fill="#F2A93B" fillOpacity="0.5" />
      <rect x="23.5" y="34.75" width="17" height="4.5" rx="2.25" fill="#FFD58A" />
      <rect x="23.5" y="41.25" width="17" height="3" rx="1.5" fill="#F2A93B" fillOpacity="0.5" />
      <rect x="20.5" y="51" width="23" height="4.5" rx="2.25" fill="currentColor" fillOpacity="0.6" />
    </svg>
  );
}

/** Drag-to-resize handle between panels. */
export function Splitter({
  direction,
  onDrag,
  onDoubleClick,
}: {
  direction: 'vertical' | 'horizontal';
  onDrag: (delta: number) => void;
  onDoubleClick?: () => void;
}) {
  const start = useRef(0);
  return (
    <div
      className={clsx('splitter', `splitter-${direction}`)}
      onDoubleClick={onDoubleClick}
      onMouseDown={(e) => {
        e.preventDefault();
        start.current = direction === 'vertical' ? e.clientX : e.clientY;
        document.body.classList.add(direction === 'vertical' ? 'is-resizing-x' : 'is-resizing-y');
        const move = (ev: MouseEvent) => {
          const cur = direction === 'vertical' ? ev.clientX : ev.clientY;
          onDrag(cur - start.current);
          start.current = cur;
        };
        const up = () => {
          document.body.classList.remove('is-resizing-x', 'is-resizing-y');
          window.removeEventListener('mousemove', move);
          window.removeEventListener('mouseup', up);
        };
        window.addEventListener('mousemove', move);
        window.addEventListener('mouseup', up);
      }}
    />
  );
}
