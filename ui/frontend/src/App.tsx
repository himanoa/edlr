import { useState } from "react";
import Dashboard from "./pages/Dashboard";
import Logs from "./pages/Logs";
import Plugins from "./pages/Plugins";

const TABS = ["Dashboard", "Logs", "Plugins"] as const;
type Tab = (typeof TABS)[number];

export default function App() {
  const [tab, setTab] = useState<Tab>("Dashboard");
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
      </main>
    </div>
  );
}
