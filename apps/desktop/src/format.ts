export type RateWindow = {
  label: string;
  usedPercent?: number;
  remainingPercent?: number;
  resetsAt?: number;
  windowMinutes?: number;
};

export function formatTokens(value: number): string {
  if (!Number.isFinite(value) || value < 0) return "0";
  if (value >= 99_500_000 && value < 100_000_000) {
    return "1亿";
  }
  if (value >= 100_000_000) {
    return `${(value / 100_000_000).toFixed(value >= 1_000_000_000 ? 0 : 1).replace(/\.0$/, "")}亿`;
  }
  if (value >= 10_000) {
    return `${(value / 10_000).toFixed(value >= 100_000 ? 0 : 1).replace(/\.0$/, "")}万`;
  }
  return Math.round(value).toLocaleString("zh-CN");
}

export function formatReset(epochSeconds?: number): string {
  if (!epochSeconds) return "等待重置时间";
  return `${new Date(epochSeconds * 1000).toLocaleString("zh-CN", {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  })} 重置`;
}

export function formatWindow(window: RateWindow): string {
  if (window.remainingPercent == null) return `${window.label || "额度"}暂不可用`;
  return `${window.label}剩余 ${Math.round(window.remainingPercent)}%`;
}
