import { useEffect, useState } from "react";
import { useShell } from "../context/AppContext";
import { api, isTauriRuntime } from "../lib/ipc";
import "./StatusBanners.css";

function StatusBanners() {
  const { backendConnected, setBackendConnected } = useShell();
  const [connecting, setConnecting] = useState(true);

  useEffect(() => {
    if (!isTauriRuntime()) {
      setConnecting(false);
      return;
    }
    let mounted = true;
    const check = () => {
      api
        .modelStatus()
        .then(() => {
          if (mounted) {
            setBackendConnected(true);
            setConnecting(false);
          }
        })
        .catch(() => {
          if (mounted) {
            setBackendConnected(false);
            setConnecting(false);
          }
        });
    };
    check();
    const interval = setInterval(check, 30000);
    return () => {
      mounted = false;
      clearInterval(interval);
    };
  }, [setBackendConnected]);

  return (
    <>
      {connecting && (
        <div className="connecting-banner" role="status">
          <span className="connecting-spinner" aria-hidden="true" />
          <span className="connecting-text">Starting backend…</span>
        </div>
      )}
    </>
  );
}

export default StatusBanners;