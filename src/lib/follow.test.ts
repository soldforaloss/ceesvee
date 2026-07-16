import { describe, expect, it } from "vitest";

import { followAlertMessage } from "./follow";

describe("followAlertMessage", () => {
  it("covers every alert kind distinctly", () => {
    const kinds = ["truncatedOrRotated", "widthChanged", "encodingChanged", "missing"] as const;
    const messages = kinds.map(followAlertMessage);
    expect(new Set(messages).size).toBe(kinds.length);
    expect(messages[0]).toContain("never combined");
  });
});
