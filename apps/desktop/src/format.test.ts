import { describe, expect, it } from "vitest";
import { formatReset, formatTokens, formatWindow } from "./format";

describe("formatTokens", () => {
  it("formats compact dashboard totals", () => {
    expect(formatTokens(999)).toBe("999");
    expect(formatTokens(1_200)).toBe("1.2K");
    expect(formatTokens(2_500_000)).toBe("2.5M");
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
});
