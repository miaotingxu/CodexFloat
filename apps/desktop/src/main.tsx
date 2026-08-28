import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow, currentMonitor, PhysicalPosition } from "@tauri-apps/api/window";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
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
  rateStatus: "live" | "stale" | "unavailable";
  rateFreshness: "live" | "stale" | "unavailable";
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
  rateFreshness: "unavailable",
  tokenCoverage: "unavailable",
  updatedAt: "",
};

function App() {
  const [snapshot, setSnapshot] = useState(fallback);
  const [loading, setLoading] = useState(true);
  const [clickThrough, setClickThrough] = useState(false);
  const snapTimer = useRef<number | undefined>(undefined);
  const snapping = useRef(false);

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

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<boolean>("click-through-changed", (event) => setClickThrough(event.payload))
      .then((dispose) => { unlisten = dispose; });
    return () => { unlisten?.(); };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void getCurrentWindow().onMoved(({ payload }) => {
      void invoke("save_window_position", { x: payload.x, y: payload.y });
      if (snapping.current) return;
      if (snapTimer.current !== undefined) window.clearTimeout(snapTimer.current);
      snapTimer.current = window.setTimeout(async () => {
        const windowHandle = getCurrentWindow();
        const monitor = await currentMonitor();
        if (!monitor) return;
        const [size, position] = await Promise.all([windowHandle.outerSize(), windowHandle.outerPosition()]);
        const area = monitor.workArea;
        const threshold = Math.round(24 * monitor.scaleFactor);
        const right = area.position.x + area.size.width - size.width;
        const bottom = area.position.y + area.size.height - size.height;
        const x = Math.abs(position.x - area.position.x) <= threshold ? area.position.x
          : Math.abs(position.x - right) <= threshold ? right : position.x;
        const y = Math.abs(position.y - area.position.y) <= threshold ? area.position.y
          : Math.abs(position.y - bottom) <= threshold ? bottom : position.y;
        if (x === position.x && y === position.y) return;
        snapping.current = true;
        try {
          await windowHandle.setPosition(new PhysicalPosition(x, y));
          await invoke("save_window_position", { x, y });
        } finally {
          snapping.current = false;
        }
      }, 180);
    }).then((dispose) => { unlisten = dispose; });
    return () => {
      if (snapTimer.current !== undefined) window.clearTimeout(snapTimer.current);
      unlisten?.();
    };
  }, []);

  const beginDrag = (event: React.MouseEvent<HTMLElement>) => {
    if (event.button !== 0) return;
    const target = event.target instanceof Element ? event.target : null;
    if (target?.closest("button, input, a, [data-no-drag]")) return;
    event.preventDefault();
    void getCurrentWindow().startDragging().catch((error) => {
      console.error("Failed to start Codex Float dragging:", error);
    });
  };

  const toggle = async () => {
    const expanded = !snapshot.expanded;
    setSnapshot((current) => ({ ...current, expanded }));
    await invoke("set_window_expanded", { expanded }).catch(() => undefined);
  };

  const primary = snapshot.windows[0];
  const maxDay = useMemo(() => Math.max(...snapshot.daily, 1), [snapshot.daily]);

  return (
    <main className={snapshot.expanded ? "expanded" : "collapsed"}>
      <section
        className="compact-row drag-handle"
        onMouseDown={beginDrag}
        onClick={toggle}
      >
        <div className="brand-mark" aria-hidden="true">C</div>
        <div className="quota-summary">
          <small>{primary?.label ?? "Codex 额度"}</small>
          <strong>{loading ? "读取中" : primary ? `${Math.round(primary.remainingPercent ?? 0)}%` : "--"}</strong>
        </div>
        <div className="token-summary">
          <small>今日 Token</small>
          <strong>{formatTokens(snapshot.today)}</strong>
        </div>
        <span className={`status-dot ${snapshot.rateFreshness}`} title={snapshot.rateFreshness === "live" ? "额度实时" : snapshot.rateFreshness === "stale" ? "额度来自最近缓存" : "额度不可用"} />
        <button
          type="button"
          className="toggle-button compact-toggle"
          aria-label={snapshot.expanded ? "收起窗口" : "展开窗口"}
          title="展开详情"
          onPointerDown={(event) => event.stopPropagation()}
          onClick={(event) => { event.stopPropagation(); void toggle(); }}
        ><svg viewBox="0 0 16 16" aria-hidden="true"><path d="m4 6 4 4 4-4" /></svg></button>
      </section>

      {snapshot.expanded && (
        <div className="details">
          <header
            className="drag-handle"
            onMouseDown={beginDrag}
            onClick={toggle}
          >
            <div>
              <b>Codex Float</b>
              <span>本机状态</span>
            </div>
            <div className="header-actions">
              <button type="button" className="toggle-button panel-toggle" aria-label="收起窗口" title="收起窗口" onPointerDown={(event) => event.stopPropagation()} onClick={(event) => { event.stopPropagation(); void toggle(); }}><svg viewBox="0 0 16 16" aria-hidden="true"><path d="m4 10 4-4 4 4" /></svg></button>
              <button type="button" onPointerDown={(event) => event.stopPropagation()} onClick={(event) => { event.stopPropagation(); void refresh(); }}>刷新</button>
            </div>
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
            <span>{clickThrough ? "鼠标穿透 · Ctrl+Shift+Space 恢复" : snapshot.rateFreshness === "stale" ? "额度来自缓存 · 正在等待实时数据" : snapshot.rateFreshness === "unavailable" ? "额度暂不可用" : snapshot.tokenCoverage === "partial" ? "部分覆盖 · 本机可观测用量" : "Token 暂不可用"}</span>
            <span className="footer-actions">
              {snapshot.updatedAt ? `更新于 ${new Date(snapshot.updatedAt).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}` : "尚未更新"}
              <button type="button" onPointerDown={(event) => event.stopPropagation()} onClick={(event) => {
                event.stopPropagation();
                if (window.confirm("清空本机 Token 统计？原始 Codex 会话文件不会被删除。")) {
                  void invoke("clear_token_history").then(refresh);
                }
              }}>清空统计</button>
            </span>
          </footer>
        </div>
      )}
    </main>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
