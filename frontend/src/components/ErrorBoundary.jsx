import React from 'react';
import { AlertTriangle, RefreshCw } from 'lucide-react';

// Reusable render-error boundary (finding 22): catches errors thrown while
// rendering/committing/lifecycle-ing a child subtree and shows a friendly
// fallback + retry instead of a blank white screen or a hard crash. Wrapped
// ad hoc around individual pages for now (e.g. ObserveDashboard) - wrapping
// the whole app in App.jsx is the natural follow-up, but that file is owned
// by another worker so it's left out of scope here.
export default class ErrorBoundary extends React.Component {
  constructor(props) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error) {
    return { hasError: true, error };
  }

  componentDidCatch(error, info) {
    console.error('ErrorBoundary caught a render error:', error, info);
  }

  handleRetry = () => {
    this.setState({ hasError: false, error: null });
  };

  render() {
    if (this.state.hasError) {
      return (
        <div className="neo-border bg-neo-red text-white p-4 flex flex-col gap-2">
          <span className="flex items-center gap-2 font-bold text-sm">
            <AlertTriangle size={16} /> Something went wrong rendering this section.
          </span>
          {this.state.error?.message && (
            <span className="text-xs font-mono opacity-90">{this.state.error.message}</span>
          )}
          <button
            onClick={this.handleRetry}
            className="neo-btn bg-white text-black py-1.5 px-3 text-xs font-bold flex items-center gap-1.5 self-start"
          >
            <RefreshCw size={13} /> Retry
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
