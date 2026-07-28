import React from "react";

/// Last-resort error boundary. Without one, any exception thrown during render or
/// in a hook unmounts the entire React root and the window goes solid black while
/// the backend keeps running — which is how a missing entry in errorSuggestionKeys
/// turned every "couldn't reach the headset" error into a black screen (2.2.13–16).
/// This keeps the crash visible, copyable, and recoverable instead.
export class CrashBoundary extends React.Component<
  { children: React.ReactNode },
  { error: Error | null }
> {
  constructor(props: { children: React.ReactNode }) {
    super(props);
    this.state = { error: null };
  }

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  render() {
    if (!this.state.error) {
      return this.props.children;
    }
    return (
      <div
        style={{
          padding: "2rem",
          fontFamily: "system-ui, sans-serif",
          color: "#eee",
          background: "#1a1a1a",
          minHeight: "100vh",
          boxSizing: "border-box",
        }}
      >
        <h2>iloader hit an internal error</h2>
        <p>
          The app itself is fine — this is a bug in its interface. Please copy
          the text below and report it, then reload.
        </p>
        <pre
          style={{
            whiteSpace: "pre-wrap",
            background: "#000",
            padding: "1rem",
            borderRadius: "6px",
            userSelect: "text",
          }}
        >
          {String(this.state.error?.stack ?? this.state.error)}
        </pre>
        <button
          style={{ padding: "0.5rem 1rem", marginTop: "0.5rem" }}
          onClick={() => window.location.reload()}
        >
          Reload iloader
        </button>
      </div>
    );
  }
}
