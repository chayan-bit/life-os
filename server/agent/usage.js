// Shared Claude Agent SDK usage-token accounting (docs/AGENT-CORE.md §7).
// Every stage (plan/execute/critique) reads the same result-message usage
// shape ({ input_tokens, output_tokens }); centralizing it keeps the
// per-turn tokens_in/tokens_out split (issue #125's Observe run-log lens)
// consistent across all three call sites instead of three ad-hoc copies.

// Total tokens for a single usage block (kept for the existing combined
// `tokens` field that gate.js's daily budget sum reads).
export function usageTokens(usage) {
  if (!usage) return 0;
  return (Number(usage.input_tokens) || 0) + (Number(usage.output_tokens) || 0);
}

// Immutable fold: returns a NEW { tokensIn, tokensOut } accumulator, never
// mutates `acc`.
export function foldUsage(acc, usage) {
  if (!usage) return acc;
  return {
    tokensIn: acc.tokensIn + (Number(usage.input_tokens) || 0),
    tokensOut: acc.tokensOut + (Number(usage.output_tokens) || 0),
  };
}

export const emptyUsage = () => ({ tokensIn: 0, tokensOut: 0 });
