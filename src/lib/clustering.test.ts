import { describe, expect, it } from "vitest";

import { buildClusterMapping, defaultDecisions, rowsAffectedByDecisions } from "./clustering";
import type { ValueCluster } from "../types";

const clusters: ValueCluster[] = [
  {
    members: [
      { value: "Acme Inc", count: 5 },
      { value: "acme inc", count: 2 },
      { value: "ACME INC.", count: 1 },
    ],
    suggested: "Acme Inc",
    matchKey: "acme inc",
    rowsAffected: 3,
  },
  {
    members: [
      { value: "New York", count: 4 },
      { value: "new york", count: 1 },
    ],
    suggested: "New York",
    matchKey: "new york",
    rowsAffected: 1,
  },
];

describe("cluster decisions (F24)", () => {
  it("defaults to nothing accepted with the suggestion prefilled", () => {
    const decisions = defaultDecisions(clusters);
    expect(decisions).toHaveLength(2);
    expect(decisions[0]).toEqual({ accepted: false, canonical: "Acme Inc" });
    expect(buildClusterMapping(clusters, decisions)).toEqual([]);
  });

  it("maps only accepted clusters, skipping the canonical member", () => {
    const decisions = defaultDecisions(clusters);
    decisions[0].accepted = true;
    const mapping = buildClusterMapping(clusters, decisions);
    expect(mapping).toEqual([
      ["acme inc", "Acme Inc"],
      ["ACME INC.", "Acme Inc"],
    ]);
    expect(rowsAffectedByDecisions(clusters, decisions)).toBe(3);
  });

  it("honours a custom canonical value, remapping every member", () => {
    const decisions = defaultDecisions(clusters);
    decisions[1] = { accepted: true, canonical: "New York, NY" };
    const mapping = buildClusterMapping(clusters, decisions);
    expect(mapping).toEqual([
      ["New York", "New York, NY"],
      ["new york", "New York, NY"],
    ]);
    expect(rowsAffectedByDecisions(clusters, decisions)).toBe(5);
  });

  it("rejected clusters contribute nothing even with edits", () => {
    const decisions = defaultDecisions(clusters);
    decisions[0] = { accepted: false, canonical: "Something Else" };
    expect(buildClusterMapping(clusters, decisions)).toEqual([]);
    expect(rowsAffectedByDecisions(clusters, decisions)).toBe(0);
  });
});
