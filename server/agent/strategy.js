// Epsilon-greedy A/B strategy optimizer (issue #138, docs/AGENT-CORE.md §13).
// A finer-grained sibling of the Release loop (docs/HARNESS-LOOP.md §4), which
// already learns one routing prior: this learns per-decision-group which
// prompt/approach variant wins by logged outcome. Outcomes ride `events`
// (type 'agent.strategy.outcome') - no new store, no new always-on cost.
//
// This module is a LIBRARY plus its events convention only. Wiring it into a
// specific live decision group (e.g. `draft_tone`) is deliberately deferred -
// there is no such decision point in the loop today, and adding one here
// would be speculative generality ahead of a real caller.

export const DEFAULT_EPSILON = 0.1;
const STRATEGY_OUTCOME_TYPE = "agent.strategy.outcome";
// Bounded fetch: /api/event has no group-column filter (group lives inside
// attrs), so this fetches the type-filtered tail and filters client-side.
const LOAD_LIMIT = 500;

// Best-effort append of one (group, variant, success) outcome. Never throws -
// mirrors executor.js's denyForbidden / loop.js's escalate pattern: the
// ledger write is auxiliary, the decision it informs is authoritative.
export async function recordOutcome(httpFn, workspaceId, group, variant, success) {
  try {
    await httpFn("POST", "/api/event", {
      type: STRATEGY_OUTCOME_TYPE,
      actor: "agent",
      attrs: { group, variant, success: Boolean(success) },
      workspace_id: workspaceId,
    });
  } catch {
    // Best-effort; a missed outcome only slightly dulls future selection.
  }
}

// Loads this group's logged outcomes. Returns [] on any failure or when
// nothing has been logged yet - a cold group must never fail the caller, it
// just has every variant unseen.
export async function loadOutcomes(httpFn, workspaceId, group) {
  try {
    const path = `/api/event?type=${encodeURIComponent(STRATEGY_OUTCOME_TYPE)}&workspace_id=${encodeURIComponent(workspaceId)}&limit=${LOAD_LIMIT}`;
    const res = await httpFn("GET", path);
    const rows = res?.ok && Array.isArray(res.data) ? res.data : [];
    return rows.map((row) => row.attrs ?? row).filter((attrs) => attrs?.group === group);
  } catch {
    return [];
  }
}

// Per-variant { plays, successes, rate }, one entry per name in `variants` -
// including variants with zero plays, so the caller can detect "unseen"
// without a second pass over outcomes.
function statsByVariant(outcomes, variants) {
  const stats = new Map(variants.map((variant) => [variant, { plays: 0, successes: 0 }]));
  for (const outcome of outcomes) {
    const entry = stats.get(outcome.variant);
    if (!entry) continue;
    entry.plays += 1;
    if (outcome.success) entry.successes += 1;
  }
  return stats;
}

function successRate(entry) {
  return entry.plays === 0 ? 0 : entry.successes / entry.plays;
}

// chooseVariant(outcomes, variants, opts) -> variant name.
// Rules (docs/AGENT-CORE.md §13, issue #138):
//   1. any variant with zero recorded outcomes explores first, in `variants`
//      order (never left to chance - a cold variant is always tried once).
//   2. else, with probability `epsilon`, explore uniformly at random.
//   3. else exploit the highest success rate; ties break to the first
//      variant in `variants` order (deterministic, not random).
// `rng` is required-injectable for tests; `Math.random` is only a runtime
// default - no bare Math.random anywhere else in this decision path.
export function chooseVariant(outcomes, variants, { epsilon = DEFAULT_EPSILON, rng = Math.random } = {}) {
  const stats = statsByVariant(outcomes, variants);
  const unseen = variants.find((variant) => stats.get(variant).plays === 0);
  if (unseen) return unseen;

  if (rng() < epsilon) {
    return variants[Math.floor(rng() * variants.length)];
  }

  return variants.reduce((best, variant) =>
    successRate(stats.get(variant)) > successRate(stats.get(best)) ? variant : best,
  variants[0]);
}

// leaderboard(outcomes, group) -> [{ variant, plays, successes, rate }],
// sorted by rate desc then plays desc. Outcomes are filtered to `group`
// here (unlike chooseVariant, which expects a pre-filtered list) so a caller
// holding a mixed multi-group outcome set can still ask for one group's view.
export function leaderboard(outcomes, group) {
  const scoped = outcomes.filter((outcome) => outcome.group === group);
  const variants = [...new Set(scoped.map((outcome) => outcome.variant))];
  const stats = statsByVariant(scoped, variants);
  return variants
    .map((variant) => {
      const entry = stats.get(variant);
      return { variant, plays: entry.plays, successes: entry.successes, rate: successRate(entry) };
    })
    .sort((a, b) => b.rate - a.rate || b.plays - a.plays);
}
