import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";

import { isTauriRuntime, tauriInvoke } from "./lib/ipc";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);

// Headless boot smoke (CI / BN-12): when the host runs with AI_EDITOR_SMOKE=1
// it expects this webview to report a successful mount, then exits green.
// Any boot-side failure (module crash, render error, failed invoke) is
// forwarded to the backend so headless runs show the root cause.
if (isTauriRuntime()) {
  const reportBootFailure = (message: string) => {
    void tauriInvoke<void>("smoke_fail", { message }).catch(() => {});
  };
  window.addEventListener("error", (e) =>
    reportBootFailure(`error: ${e.message} @ ${e.filename}:${e.lineno}:${e.colno}`),
  );
  window.addEventListener("unhandledrejection", (e) =>
    reportBootFailure(`rejection: ${String(e.reason)}`),
  );
  tauriInvoke<boolean>("smoke_active")
    .then((active) => {
      if (active) return tauriInvoke<void>("smoke_boot_ok");
    })
    .catch((e) => reportBootFailure(`smoke invoke failed: ${String(e)}`));
}
