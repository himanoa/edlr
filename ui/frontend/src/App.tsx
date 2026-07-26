import { useEffect, useState } from "react";
import { invoke, isTauri, type AppConfigDto } from "./lib/tauri";
import Dashboard from "./pages/Dashboard";
import Logs from "./pages/Logs";
import Plugins from "./pages/Plugins";
import Settings from "./pages/Settings";

const TABS = ["Dashboard", "Logs", "Plugins", "Settings"] as const;
type Tab = (typeof TABS)[number];

export default function App() {
  const [tab, setTab] = useState<Tab>("Dashboard");

  // journal_dir が未設定なら Settings から始める。デーモンが居ない状態で
  // Dashboard を出しても何も表示できないため。
  useEffect(() => {
    if (!isTauri()) return;
    let active = true;
    invoke<AppConfigDto>("get_config")
      .then((config) => {
        if (active && config.journalDir === null) setTab("Settings");
      })
      .catch(() => {
        // 取得に失敗しても既定タブのまま続行する
      });
    return () => {
      active = false;
    };
  }, []);

  return (
    <div className="app">
      <nav className="tabs">
        {TABS.map((t) => (
          <button
            key={t}
            className={t === tab ? "tab active" : "tab"}
            onClick={() => setTab(t)}
          >
            {t}
          </button>
        ))}
      </nav>
      <main className="page">
        {tab === "Dashboard" && <Dashboard />}
        {tab === "Logs" && <Logs />}
        {tab === "Plugins" && <Plugins />}
        {tab === "Settings" && <Settings />}
      </main>
    </div>
  );
}
