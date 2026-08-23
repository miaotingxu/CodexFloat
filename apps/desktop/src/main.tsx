import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import { formatReset, formatTokens, formatWindow, type RateWindow } from "./format";
import "./style.css";

type Snapshot = {
  expanded: boolean;
  today: number;
  last7: number;
  last30: number;
  daily: number[];
  windows: RateWindow[];
  rateStatus: "live" | "unavailable";
  tokenCoverage: "partial" | "unavailable";
  updatedAt: string;
};

const fallback: Snapshot = {
  expanded: false,
  today: 0,
  last7: 0,
  last30: 0,
  daily: [],
  windows: [],
  rateStatus: "unavailable",
  tokenCoverage: "unavailable",
  updatedAt: "",
};

function App() {
  const [snapshot, setSnapshot] = useState(fallback);
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(async () => {
    try {
      setSnapshot(await invoke<Snapshot>("get_dashboard_snapshot"));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 120_000);
    return () => window.clearInterval(timer);
  }, [refresh]);

  const toggle = async () => {
    const expanded = !snapshot.expanded;
    setSnapshot((current) => ({ ...current, expanded }));
    await invoke("set_window_expanded", { expanded }).catch(() => undefined);
  };

  const primary = snapshot.windows[0];
  const maxDay = useMemo(() => Math.max(...snapshot.daily, 1), [snapshot.daily]);

  return (
    <main className={snapshot.expanded ? "expanded" : "collapsed"} onClick={toggle}>
      <section className="compact-row">
        <div className="brand-mark" aria-hidden="true">C</div>
        <div className="quota-summary">
          <small>{primary?.label ?? "Codex 额度"}</small>
          <strong>{loading ? "读取中" : primary ? `${Math.round(primary.remainingPercent ?? 0)}%` : "--"}</strong>
        </div>
        <div className="token-summary">
          <small>今日 Token</small>
          <strong>{formatTokens(snapshot.today)}</strong>
        </div>
        <span className={`status-dot ${snapshot.rateStatus}`} title={snapshot.rateStatus === "live" ? "额度实时" : "额度不可用"} />
      </section>

      {snapshot.expanded && (
        <div className="details">
          <header>
            <div>
              <b>Codex Float</b>
              <span>本机状态</span>
            </div>
            <button type="button" onClick={(event) => { event.stopPropagation(); void refresh(); }}>刷新</button>
          </header>

          <section className="quota-list">
            {snapshot.windows.length ? snapshot.windows.map((window, index) => (
              <div className="quota-card" key={`${window.label}-${index}`}>
                <span>{formatWindow(window)}</span>
                <small>{formatReset(window.resetsAt)}</small>
                <i><em style={{ width: `${window.remainingPercent ?? 0}%` }} /></i>
              </div>
            )) : <div className="empty">额度暂不可用</div>}
          </section>

          <section className="totals">
            <div><small>今天</small><strong>{formatTokens(snapshot.today)}</strong></div>
            <div><small>最近 7 天</small><strong>{formatTokens(snapshot.last7)}</strong></div>
            <div><small>最近 30 天</small><strong>{formatTokens(snapshot.last30)}</strong></div>
          </section>

          <section className="trend">
            <div className="bars">
              {snapshot.daily.slice(-7).map((tokens, index) => (
                <i key={index} title={formatTokens(tokens)} style={{ height: `${Math.max(7, tokens / maxDay * 100)}%` }} />
              ))}
            </div>
          </section>

          <footer>
            <span>{snapshot.tokenCoverage === "partial" ? "部分覆盖 · 本机可观测用量" : "Token 暂不可用"}</span>
            <span>{snapshot.updatedAt ? `更新于 ${new Date(snapshot.updatedAt).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}` : "尚未更新"}</span>
          </footer>
        </div>
      )}
    </main>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
