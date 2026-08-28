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
  daily: { day: string; tokens: number }[];
  windows: RateWindow[];
  account: {
    bound: boolean;
    email?: string;
    planType?: string;
    subscriptionActiveUntil?: string;
    subscriptionState: string;
    subscriptionError?: string;
    checkedAt?: string;
  };
  rateStatus: "live" | "stale" | "unavailable";
  rateFreshness: "live" | "stale" | "unavailable";
  tokenCoverage: "partial" | "unavailable" | "account";
  usageSource: "account" | "account_history_local_today" | "local";
  updatedAt: string;
};

const fallback: Snapshot = {
  expanded: false,
  today: 0,
  last7: 0,
  last30: 0,
  daily: [],
  windows: [],
  account: { bound: false, subscriptionState: "unknown" },
  rateStatus: "unavailable",
  rateFreshness: "unavailable",
  tokenCoverage: "unavailable",
  usageSource: "local",
  updatedAt: "",
};

function FloatApp() {
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
  const maxDay = useMemo(() => Math.max(...snapshot.daily.map((point) => point.tokens), 1), [snapshot.daily]);
  const quotaColor = (remaining: number | null | undefined) => {
    const percent = Math.max(0, Math.min(100, remaining ?? 0));
    return `hsl(${percent * 1.2} 72% 56%)`;
  };
  const subscriptionLabel = () => {
    const raw = snapshot.account.subscriptionActiveUntil;
    if (!raw) return snapshot.account.bound ? "订阅到期：暂不可用" : "未绑定 Codex 账号";
    const date = new Date(raw);
    if (Number.isNaN(date.getTime())) return "订阅到期：日期无效";
    const dateText = date.toLocaleDateString("zh-CN", { year: "numeric", month: "2-digit", day: "2-digit" });
    const days = Math.ceil((date.getTime() - Date.now()) / 86_400_000);
    if (days <= 0) return `订阅已到期 · ${dateText}`;
    return `订阅到期：${dateText} · 剩余 ${days} 天`;
  };
  const accountPlan = snapshot.account.planType ? ` · ${snapshot.account.planType}` : "";
  const subscriptionClass = snapshot.account.subscriptionState;

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
            <div className="header-account" title={snapshot.account.subscriptionError ?? snapshot.account.email ?? "未绑定账号"}>
              <strong>{snapshot.account.email ?? "未绑定账号"}</strong>
              <small className={`subscription ${subscriptionClass}`}>{subscriptionLabel()}{accountPlan}</small>
            </div>
            <div className="header-actions">
              <button type="button" className="toggle-button panel-toggle" aria-label="收起窗口" title="收起窗口" onPointerDown={(event) => event.stopPropagation()} onClick={(event) => { event.stopPropagation(); void toggle(); }}><svg viewBox="0 0 16 16" aria-hidden="true"><path d="m4 10 4-4 4 4" /></svg></button>
              <button type="button" className="refresh-button" aria-label="刷新数据" title="刷新数据" onPointerDown={(event) => event.stopPropagation()} onClick={(event) => { event.stopPropagation(); void refresh(); }}><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M13 5V2.8L11.2 4.6A5 5 0 1 0 12.8 10" /></svg></button>
            </div>
          </header>

          <section className="quota-list">
            {snapshot.windows.length ? snapshot.windows.map((window, index) => (
              <div className="quota-card" key={`${window.label}-${index}`}>
                <span>{formatWindow(window)}</span>
                <small>{formatReset(window.resetsAt)}</small>
                <i><em style={{ width: `${Math.max(0, Math.min(100, window.remainingPercent ?? 0))}%`, color: quotaColor(window.remainingPercent), background: quotaColor(window.remainingPercent) }} /></i>
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
              {snapshot.daily.slice(-7).map((point) => (
                <div className="bar-item" key={point.day} title={`${point.day} · ${formatTokens(point.tokens)} Token`}><i style={{ height: `${Math.max(7, point.tokens / maxDay * 100)}%` }} /><small className="bar-day">{point.day.slice(5).replace("-", "/")}</small><small>{formatTokens(point.tokens)}</small></div>
              ))}
            </div>
          </section>

          <footer>
            <span>{clickThrough ? "鼠标穿透 · Ctrl+Shift+Space 恢复" : snapshot.usageSource === "account" ? "账户实时 Token 用量" : snapshot.usageSource === "account_history_local_today" ? "账户历史用量 · 今日为本机实时" : snapshot.rateFreshness === "stale" ? "额度来自缓存 · Token 使用本机回退" : snapshot.rateFreshness === "unavailable" ? "账户用量暂不可用 · 当前为本机回退" : "本机可观测用量"}</span>
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

function MainWindow() {
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

  const primary = snapshot.windows[0];
  const account = snapshot.account;
  const subscription = account.subscriptionActiveUntil
    ? new Date(account.subscriptionActiveUntil).toLocaleDateString("zh-CN", { year: "numeric", month: "2-digit", day: "2-digit" })
    : "暂不可用";
  const statusText = account.bound ? (account.subscriptionState === "active" ? "账号正常" : account.subscriptionState === "unknown" ? "日期待确认" : "订阅需关注") : "未绑定账号";

  return (
    <main className="dashboard-shell">
      <header className="dashboard-titlebar" data-no-drag>
        <div className="dashboard-brand"><div className="brand-mark">C</div><div><b>Codex Float</b><span>账号与用量中心</span></div></div>
        <div className="window-actions"><button aria-label="最小化" onClick={() => void getCurrentWindow().minimize()}>−</button><button aria-label="关闭主窗口" onClick={() => void getCurrentWindow().close()}>×</button></div>
      </header>
      <div className="dashboard-content">
        <section className="hero-card">
          <div><span className="eyebrow">当前账号</span><h1>{account.email ?? "未绑定 Codex 账号"}</h1><p>{account.planType ? `${account.planType} 方案 · ` : ""}{statusText}</p></div>
          <button className="dashboard-refresh" onClick={() => void refresh()} disabled={loading} aria-label="刷新数据">↻</button>
        </section>
        <section className="dashboard-grid">
          <article className="dashboard-card quota-hero"><div className="card-heading"><span>实时额度</span><i className={`live-indicator ${snapshot.rateFreshness}`} /></div><strong>{primary ? `${Math.round(primary.remainingPercent ?? 0)}%` : "--"}</strong><div className="large-progress"><i style={{ width: `${Math.max(0, Math.min(100, primary?.remainingPercent ?? 0))}%` }} /></div><small>{primary?.label ?? "额度暂不可用"} · {primary ? formatReset(primary.resetsAt) : "等待数据"}</small></article>
          <article className="dashboard-card"><div className="card-heading"><span>订阅到期</span><span className={`badge ${account.subscriptionState}`}>{account.bound ? "已绑定" : "未绑定"}</span></div><strong className="date-value">{subscription}</strong><small>{account.subscriptionError ?? (account.checkedAt ? `最近检查 ${new Date(account.checkedAt).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}` : "等待检查")}</small></article>
          <article className="dashboard-card"><div className="card-heading"><span>今日 Token</span><span className="muted-label">{snapshot.usageSource === "account" ? "账户实时" : snapshot.usageSource === "account_history_local_today" ? "账户历史 · 本机今日" : "本机回退"}</span></div><strong>{formatTokens(snapshot.today)}</strong><small>最近 7 天 {formatTokens(snapshot.last7)}</small></article>
        </section>
        <section className="dashboard-card usage-card"><div className="section-heading"><div><span className="eyebrow">Usage trend</span><h2>Token 使用趋势</h2></div><span className="muted-label">最近 7 天</span></div><div className="dashboard-bars">{snapshot.daily.slice(-7).map((point) => { const max = Math.max(...snapshot.daily.map((item) => item.tokens), 1); return <div className="dashboard-bar" key={point.day}><i style={{ height: `${Math.max(8, point.tokens / max * 100)}%` }} /><small>{point.day.slice(5).replace("-", "/")}</small><b>{formatTokens(point.tokens)}</b></div>; })}</div></section>
        <section className="dashboard-card windows-card"><div className="section-heading"><div><span className="eyebrow">Quota windows</span><h2>额度窗口</h2></div><span className={`badge ${snapshot.rateFreshness}`}>{snapshot.rateFreshness === "live" ? "实时" : snapshot.rateFreshness === "stale" ? "缓存" : "不可用"}</span></div><div className="window-list">{snapshot.windows.map((item, index) => <div className="window-row" key={`${item.label}-${index}`}><span>{formatWindow(item)}</span><b>{Math.round(item.remainingPercent ?? 0)}%</b><small>{formatReset(item.resetsAt)}</small></div>)}</div></section>
      </div>
      <div className="dashboard-corner-badge"><i />{account.bound ? "实时同步" : "等待绑定"}</div>
    </main>
  );
}

const Root = getCurrentWindow().label === "main" ? MainWindow : FloatApp;
createRoot(document.getElementById("root")!).render(<Root />);
