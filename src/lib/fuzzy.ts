// Small, dependency-free fuzzy matcher for the command palette (F11).
//
// Scoring favours what people actually type: prefix hits beat word-boundary
// hits beat scattered subsequence hits, consecutive runs are rewarded, and
// gaps are penalised. Deterministic: equal scores keep the input order.

export interface FuzzyMatch {
  score: number;
  /** Indices into the HAYSTACK of the matched characters (for highlighting). */
  positions: number[];
}

const SCORE_CONSECUTIVE = 8;
const SCORE_WORD_BOUNDARY = 10;
const SCORE_PREFIX_BONUS = 12;
const SCORE_SUBSTRING_BONUS = 15;
const GAP_PENALTY = 1;

function isBoundary(haystack: string, index: number): boolean {
  if (index === 0) return true;
  const prev = haystack[index - 1];
  return prev === " " || prev === "-" || prev === "_" || prev === "/" || prev === ".";
}

/**
 * Match `query` against `haystack` as a case-insensitive subsequence.
 * Returns null when any query character cannot be placed in order.
 */
export function fuzzyMatch(query: string, haystack: string): FuzzyMatch | null {
  const q = query.toLowerCase();
  const h = haystack.toLowerCase();
  if (q.length === 0) return { score: 0, positions: [] };
  if (q.length > h.length) return null;

  const scattered = fuzzySubsequence(q, h, haystack);
  const contiguous = substringMatch(q, h, haystack);
  if (scattered && contiguous) {
    return contiguous.score >= scattered.score ? contiguous : scattered;
  }
  return contiguous ?? scattered;
}

/** An exact substring occurrence, scored so it dominates scattered letters
 * hitting the same characters across word boundaries. */
function substringMatch(q: string, h: string, haystack: string): FuzzyMatch | null {
  const at = h.indexOf(q);
  if (at === -1) return null;
  let score = SCORE_SUBSTRING_BONUS + (q.length - 1) * SCORE_CONSECUTIVE;
  if (isBoundary(haystack, at)) score += SCORE_WORD_BOUNDARY;
  if (at === 0) score += SCORE_PREFIX_BONUS;
  score -= at * GAP_PENALTY;
  score -= Math.floor(h.length / 8);
  return { score, positions: Array.from({ length: q.length }, (_, i) => at + i) };
}

function fuzzySubsequence(q: string, h: string, haystack: string): FuzzyMatch | null {
  const positions: number[] = [];
  let score = 0;
  let hi = 0;
  let lastHit = -2;

  for (let qi = 0; qi < q.length; qi++) {
    const ch = q[qi];
    // Greedy: prefer the next boundary occurrence of ch over the very next
    // occurrence, so "cd" matches the C and D of "Copy Data" rather than
    // C + the 'd' inside "Copy".
    let found = -1;
    let boundaryFound = -1;
    for (let i = hi; i < h.length; i++) {
      if (h[i] !== ch) continue;
      if (found === -1) found = i;
      if (isBoundary(haystack, i)) {
        boundaryFound = i;
        break;
      }
      // A consecutive continuation is as good as a boundary; stop looking.
      if (i === lastHit + 1) break;
    }
    const pick = boundaryFound !== -1 && found !== lastHit + 1 ? boundaryFound : found;
    if (pick === -1) return null;

    if (pick === lastHit + 1) score += SCORE_CONSECUTIVE;
    if (isBoundary(haystack, pick)) score += SCORE_WORD_BOUNDARY;
    if (pick === qi) score += SCORE_PREFIX_BONUS; // aligned with the very start
    score -= Math.max(0, pick - hi) * GAP_PENALTY;

    positions.push(pick);
    lastHit = pick;
    hi = pick + 1;
  }

  // Shorter haystacks win ties: "Sort" over "Sort descending" for "sort".
  score -= Math.floor(h.length / 8);
  return { score, positions };
}

/**
 * Score `query` against a title plus optional keywords; the best source wins.
 * Keyword hits rank slightly below equivalent title hits.
 */
export function fuzzyScore(query: string, title: string, keywords?: string[]): number | null {
  const titleMatch = fuzzyMatch(query, title);
  let best: number | null = titleMatch ? titleMatch.score : null;
  if (keywords) {
    for (const keyword of keywords) {
      const m = fuzzyMatch(query, keyword);
      if (m !== null) {
        const score = m.score - 2;
        if (best === null || score > best) best = score;
      }
    }
  }
  return best;
}
