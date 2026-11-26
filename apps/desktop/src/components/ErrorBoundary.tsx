import { AlertTriangle, Copy, RotateCcw } from 'lucide-react';
import { Component, type ErrorInfo, type ReactNode } from 'react';

interface Props {
  children: ReactNode;
  /** Shown in the fallback, e.g. "this tab". */
  label?: string;
  /** Changing this value resets the boundary. */
  resetKey?: unknown;
  compact?: boolean;
}

interface State {
  error: Error | null;
  stack: string;
}

/**
 * Keeps a rendering bug in one tab (or panel) from blanking the whole app.
 * Database work is unaffected: staged edits in other tabs and open
 * connections stay as they were.
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, stack: '' };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    this.setState({ stack: `${error.stack ?? error.message}\n${info.componentStack ?? ''}` });
    console.error('Rowglow UI error', error, info.componentStack);
  }

  componentDidUpdate(prev: Props) {
    if (this.state.error && prev.resetKey !== this.props.resetKey) this.setState({ error: null, stack: '' });
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className={this.props.compact ? 'crash crash-compact' : 'crash'}>
        <AlertTriangle size={20} />
        <div className="crash-title">Something went wrong in {this.props.label ?? 'this view'}</div>
        <div className="crash-msg">{this.state.error.message}</div>
        <div className="crash-actions">
          <button className="btn btn-sm btn-primary" onClick={() => this.setState({ error: null, stack: '' })}>
            <RotateCcw size={13} /> Try again
          </button>
          <button
            className="btn btn-sm"
            onClick={() => navigator.clipboard?.writeText(`Rowglow error: ${this.state.error?.message}\n\n${this.state.stack}`)}
          >
            <Copy size={13} /> Copy details
          </button>
        </div>
      </div>
    );
  }
}
