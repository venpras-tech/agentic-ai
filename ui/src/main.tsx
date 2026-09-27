import React from "react";
import ReactDOM from "react-dom/client";
import { HashRouter } from "react-router-dom";
import App from "./App";
import { AppProvider } from "./context/AppContext";
import { isTauriRuntime, tauriInvoke } from "./lib/ipc";
import "./styles/index.css";

const rootEl = document.getElementById("root");
if (rootEl) {
  ReactDOM.createRoot(rootEl).render(
    <React.StrictMode>
      <HashRouter>
        <AppProvider>
          <App />
        </AppProvider>
      </HashRouter>
    </React.StrictMode>,
  );
}

if (isTauriRuntime()) {
  void tauriInvoke<boolean>("smoke_active")
    .then((active) => {
      if (active) return tauriInvoke<void>("smoke_boot_ok");
    })
    .catch(() => {});
}
