// Side-effect-free tool-routing regression suite (issue #140,
// docs/AGENT-CORE.md §13, docs/HARNESS-LOOP.md §2). Complements the
// quality-focused eval-gate (docs/HARNESS-LOOP.md §2, `HaikuJudge`) with a
// routing-focused one: for each golden scenario, does the loop reach a
// sensible tool (`expect_any`) and avoid a clearly wrong one (`forbid`)?
// Runs the REAL agent loop (`runAgentTurn`) in `dryRun` mode (issue #140's
// executor/loop change) so every tool call lands in the ledger with a
// synthetic result and NEVER touches lifeos-api or the model provider's
// billed side effects - inspecting the model's tool choice, not its output.
//
// `node evals/run.js [--scenarios path]` uses the real keyless-CLI queryFn
// (runAgentTurn's own default) unless a queryFn is injected - vitest always
// injects one, so `npm test` never reaches a live model.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { z } from "zod";
import { runAgentTurn } from "../agent/loop.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export const PASS_THRESHOLD = 0.8;
export const DEFAULT_SCENARIOS_PATH = path.join(__dirname, "scenarios.json");
export const DEFAULT_HISTORY_PATH = path.join(__dirname, "history.jsonl");
const DEFAULT_WORKSPACE_ID = "ws_eval";

export const ScenarioSchema = z.object({
  id: z.string().min(1),
  prompt: z.string().min(1),
  expect_any: z.array(z.string()),
  forbid: z.array(z.string()),
});
export const ScenariosSchema = z.array(ScenarioSchema).min(1);

// Reads + validates the scenario set. Throws with a clear message on a
// malformed file - a bad scenarios.json must fail loudly at load time, not
// silently score zero scenarios.
export function loadScenarios(scenariosPath = DEFAULT_SCENARIOS_PATH) {
  const raw = fs.readFileSync(scenariosPath, "utf8");
  const parsed = ScenariosSchema.safeParse(JSON.parse(raw));
  if (!parsed.success) {
    throw new Error(`invalid scenarios file '${scenariosPath}': ${parsed.error.message}`);
  }
  return parsed.data;
}

// scoreScenario(scenario, ledger) -> { passed, matchedTools, forbiddenHit }.
// A scenario with an empty expect_any (a pure refusal check, e.g. a trading
// order request) passes as long as no forbidden tool was called - there is
// nothing it must positively call.
export function scoreScenario(scenario, ledger) {
  const toolsCalled = (ledger ?? []).map((entry) => entry.tool);
  const forbiddenHit = toolsCalled.find((tool) => scenario.forbid.includes(tool)) ?? null;
  const matchedTools = [...new Set(toolsCalled.filter((tool) => scenario.expect_any.includes(tool)))];
  const satisfiesExpectation = scenario.expect_any.length === 0 || matchedTools.length > 0;
  return { passed: satisfiesExpectation && !forbiddenHit, matchedTools, forbiddenHit };
}

// Runs one scenario through the real loop in dry-run mode and scores it.
export async function runScenario(scenario, opts = {}) {
  const turnOpts = {
    queryFn: opts.queryFn,
    httpFn: opts.httpFn,
    model: opts.model,
    dryRun: true,
  };
  const result = await runAgentTurn(scenario.prompt, opts.workspaceId ?? DEFAULT_WORKSPACE_ID, turnOpts);
  const score = scoreScenario(scenario, result.ledger);
  return { id: scenario.id, ...score, ledger: result.ledger ?? [] };
}

// Runs every scenario and aggregates a pass rate. Never throws on a single
// scenario's failure - a scenario that errors is scored as a fail, not a
// crash of the whole suite.
export async function runEval(scenarios, opts = {}) {
  const results = [];
  for (const scenario of scenarios) {
    try {
      results.push(await runScenario(scenario, opts));
    } catch (error) {
      results.push({ id: scenario.id, passed: false, matchedTools: [], forbiddenHit: null, error: error.message });
    }
  }
  const total = results.length;
  const passed = results.filter((r) => r.passed).length;
  const rate = total === 0 ? 0 : passed / total;
  const failures = results.filter((r) => !r.passed).map((r) => r.id);
  return { total, passed, rate, failures, results };
}

// { ts, total, passed, rate, failures } - the reference's committed-history
// intent, kept uncommitted-by-default here (server/evals/history.jsonl is
// gitignored) so local/CI eval runs don't create commit noise; the shape
// still mirrors what a future committed history would look like.
export function appendHistory(historyPath, summary) {
  const record = { ts: Date.now(), total: summary.total, passed: summary.passed, rate: summary.rate, failures: summary.failures };
  fs.appendFileSync(historyPath, `${JSON.stringify(record)}\n`);
  return record;
}

// Pure exit-code decision - >= threshold is green, wired as the optional
// CI/self-evolution gate (docs/AGENT-CORE.md §13).
export function exitCodeForRate(rate, threshold = PASS_THRESHOLD) {
  return rate >= threshold ? 0 : 1;
}

function parseScenariosPathArg(argv) {
  const flagIndex = argv.indexOf("--scenarios");
  return flagIndex >= 0 && argv[flagIndex + 1] ? argv[flagIndex + 1] : DEFAULT_SCENARIOS_PATH;
}

async function main() {
  const scenariosPath = parseScenariosPathArg(process.argv.slice(2));
  const scenarios = loadScenarios(scenariosPath);
  const summary = await runEval(scenarios);
  const record = appendHistory(DEFAULT_HISTORY_PATH, summary);
  console.log(JSON.stringify({ ...summary, ts: record.ts }));
  process.exitCode = exitCodeForRate(summary.rate);
}

if (process.argv[1] === import.meta.filename) {
  await main();
}
