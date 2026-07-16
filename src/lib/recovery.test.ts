import { describe, expect, it } from "vitest";

import type { RecoverableSession } from "../types";
import { recoveryAction, recoveryTime } from "./recovery";

const session = (patch: Partial<RecoverableSession>): RecoverableSession => ({
  journalPath: "j",
  sourcePath: "s.csv",
  fileName: "s.csv",
  lastEditEpochSecs: 1_700_000_000,
  operationCount: 3,
  sourceChanged: false,
  sourceMissing: false,
  incompatible: false,
  ...patch,
});

describe("recoveryAction", () => {
  it("blind replay only when the source is unchanged", () => {
    expect(recoveryAction(session({}))).toBe("recover");
    expect(recoveryAction(session({ sourceChanged: true }))).toBe("openCopy");
    expect(recoveryAction(session({ sourceMissing: true }))).toBe("none");
    expect(recoveryAction(session({ incompatible: true }))).toBe("none");
  });
});

describe("recoveryTime", () => {
  it("renders only real timestamps", () => {
    expect(recoveryTime(0)).toBe("");
    expect(recoveryTime(1_700_000_000)).not.toBe("");
  });
});
