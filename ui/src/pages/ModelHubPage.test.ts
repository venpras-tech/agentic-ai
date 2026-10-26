import { describe, it, expect } from "vitest";
import { levelFor, ramCapacityLevel } from "../lib/modelHubUtils";

describe("levelFor", () => {
  it("is green with plenty of headroom", () => {
    expect(levelFor(16, 32)).toBe("ok");
    expect(levelFor(31.9, 32)).toBe("ok");
  });

  it("is yellow when under 15% free", () => {
    expect(levelFor(4.8, 32)).toBe("warn");
    expect(levelFor(3.2, 32)).toBe("warn");
  });

  // Boundaries are inclusive on the worse level: exactly 5% free is critical.
  it("is red at or under 5% free", () => {
    expect(levelFor(1.6, 32)).toBe("crit");
    expect(levelFor(1.59, 32)).toBe("crit");
    expect(levelFor(0, 32)).toBe("crit");
  });

  it("clamps a nonsensical negative free value to critical", () => {
    expect(levelFor(-5, 32)).toBe("crit");
  });

  it("clamps an over-reported free value to ok", () => {
    expect(levelFor(64, 32)).toBe("ok");
  });

  it("does not treat a zero total as critical", () => {
    expect(levelFor(0, 0)).toBe("warn");
  });

  it("falls back to warn on non-finite input", () => {
    expect(levelFor(NaN, 32)).toBe("warn");
    expect(levelFor(8, Infinity)).toBe("warn");
  });

  it("reports a full disk as critical", () => {
    expect(levelFor(0, 20.5)).toBe("crit");
  });
});

describe("ramCapacityLevel", () => {
  it("is green for a normal machine", () => {
    expect(ramCapacityLevel(16)).toBe("ok");
    expect(ramCapacityLevel(8)).toBe("ok");
  });

  it("is yellow under 8 GB", () => {
    expect(ramCapacityLevel(7.9)).toBe("warn");
  });

  it("is red under 4 GB", () => {
    expect(ramCapacityLevel(3.9)).toBe("crit");
  });
});
