// UI-side helpers for fuzzy value clustering (F24). Pure and unit-tested;
// the dialog stays thin.

import type { ValueCluster } from "../types";

/** Per-cluster user decisions, keyed by the cluster's index in the report. */
export interface ClusterDecision {
  accepted: boolean;
  /** The canonical value to map members onto (defaults to the suggestion). */
  canonical: string;
}

/** Default decisions for a fresh report: nothing accepted, suggestions in. */
export function defaultDecisions(clusters: ValueCluster[]): ClusterDecision[] {
  return clusters.map((cluster) => ({ accepted: false, canonical: cluster.suggested }));
}

/**
 * Build the `from → to` mapping for every ACCEPTED cluster. Members equal to
 * the canonical value are skipped; rejected clusters contribute nothing.
 */
export function buildClusterMapping(
  clusters: ValueCluster[],
  decisions: ClusterDecision[],
): [string, string][] {
  const mapping: [string, string][] = [];
  clusters.forEach((cluster, index) => {
    const decision = decisions[index];
    if (!decision?.accepted) return;
    const canonical = decision.canonical;
    for (const member of cluster.members) {
      if (member.value !== canonical) mapping.push([member.value, canonical]);
    }
  });
  return mapping;
}

/** Rows a set of decisions would change (for the Apply button label). */
export function rowsAffectedByDecisions(
  clusters: ValueCluster[],
  decisions: ClusterDecision[],
): number {
  let total = 0;
  clusters.forEach((cluster, index) => {
    const decision = decisions[index];
    if (!decision?.accepted) return;
    for (const member of cluster.members) {
      if (member.value !== decision.canonical) total += member.count;
    }
  });
  return total;
}
