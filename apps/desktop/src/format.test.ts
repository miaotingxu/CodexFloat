import { describe, expect, it } from "vitest";
import { formatReset, formatResetCountdown, formatTokens, formatWindow } from "./format";

describe("formatTokens", () => {
  it("formats compact dashboard totals", () => {
    expect(formatTokens(999)).toBe("999");
    expect(formatTokens(1_200)).toBe("1,200");
    expect(formatTokens(2_500_000)).toBe("250万");
  });

  it("uses 万 between ten-thousand and hundred-million", () => {
    expect(formatTokens(9_999)).toBe("9,999");
    expect(formatTokens(10_000)).toBe("1万");
    expect(formatTokens(25_000)).toBe("2.5万");
    expect(formatTokens(100_000)).toBe("10万");
    expect(formatTokens(42_000_000)).toBe("4200万");
  });

  it("uses 亿 for hundred-million-scale totals", () => {
    expect(formatTokens(99_999_999)).toBe("1亿");
    expect(formatTokens(100_000_000)).toBe("1亿");
    expect(formatTokens(250_000_000)).toBe("2.5亿");
    expect(formatTokens(1_200_000_000)).toBe("12亿");
  });

  it("does not leak invalid values into the UI", () => {
    expect(formatTokens(Number.NaN)).toBe("0");
    expect(formatTokens(-10)).toBe("0");
  });
});

describe("quota formatting", () => {
  it("uses the actual returned window and remaining percentage", () => {
    expect(formatWindow({ label: "7 天", remainingPercent: 24.6 })).toBe("7 天剩余 25%");
  });

  it("formats app-server epoch seconds as a reset hint", () => {
    expect(formatReset(1_893_499_200)).toContain("重置");
  });

  it("shows a compact reset countdown for collapsed and expanded views", () => {
    const now = Date.parse("2026-09-30T12:00:00Z");
    const resetAt = (value: string) => Date.parse(value) / 1000;
    expect(formatResetCountdown(resetAt("2026-09-30T12:05:59Z"), now)).toBe("剩余 6 分钟");
    expect(formatResetCountdown(resetAt("2026-09-30T13:00:00Z"), now)).toBe("剩余 1 小时");
    expect(formatResetCountdown(resetAt("2026-10-01T13:00:00Z"), now)).toBe("剩余 1 天 1 小时");
    expect(formatResetCountdown(resetAt("2026-09-30T11:59:00Z"), now)).toBe("即将重置");
    expect(formatResetCountdown(undefined, now)).toBe("等待重置");
  });
});
