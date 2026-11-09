import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "../src/App";
import { StoreProvider } from "../src/store";
import "../src/styles.css";
import "./preview.css";

function TitleBar() {
  return (
    <div className="win-titlebar" aria-hidden="true">
      <span className="win-app"><span className="win-icon" />K-92</span>
      <span className="win-controls">
        <svg viewBox="0 0 10 10"><path d="M1 5h8" /></svg>
        <svg viewBox="0 0 10 10"><rect x="1.5" y="1.5" width="7" height="7" rx="1" /></svg>
        <svg viewBox="0 0 10 10"><path d="M1.5 1.5l7 7M8.5 1.5l-7 7" /></svg>
      </span>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <div className="preview-shell">
      <div className="preview-note">
        <strong>K-92 for Windows</strong>
        <span>Interactive preview of the real app UI with sample data. Nothing here touches real files.</span>
        <a href="#setup" className="preview-link">Show setup wizard</a>
        <a href="#app" className="preview-link">Show app</a>
      </div>
      <div className="win">
        <TitleBar />
        <div className="win-body">
          <StoreProvider>
            <App />
          </StoreProvider>
        </div>
      </div>
    </div>
  </StrictMode>,
);
