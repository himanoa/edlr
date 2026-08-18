import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { invoke, isTauri, type AppConfigDto } from "./lib/tauri";
import Dashboard from "./pages/Dashboard";
import DriversPage from "./pages/Drivers";
import Logs from "./pages/Logs";
import Plugins from "./pages/Plugins";
import Profiler from "./pages/Profiler";
import Settings from "./pages/Settings";

const TABS = ["Dashboard", "Logs", "Profiler", "Plugins", "Drivers", "Settings"] as const;
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
    <div className="flex h-screen flex-col bg-background text-foreground">
      <nav className="flex gap-1 border-b p-2">
        {TABS.map((t) => (
          <Button
            key={t}
            variant={t === tab ? "secondary" : "ghost"}
            size="sm"
            className={t === tab ? "active" : undefined}
            onClick={() => setTab(t)}
          >
            {t}
          </Button>
        ))}
      </nav>
      <main className="flex-1 overflow-auto p-4">
        {tab === "Dashboard" && <Dashboard />}
        {tab === "Logs" && <Logs />}
        {tab === "Profiler" && <Profiler />}
        {tab === "Plugins" && <Plugins />}
        {tab === "Drivers" && <DriversPage />}
        {tab === "Settings" && <Settings />}
      </main>
    </div>
  );
}
