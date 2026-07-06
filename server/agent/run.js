// CLI entry point (issue #122): `lifeos-api`'s /api/agent route spawns this
// exact process (`node agent/run.js <prompt> <workspaceId>`) from the server/
// dir, the same process contract `scaffold.js` has with lifeos-drain. The LAST
// non-empty stdout line is `runAgentTurn`'s return value, JSON-encoded, so the
// Rust side has one stable line to parse.
//
// `--dry-run` (issue #144) threads `opts.dryRun` into `runAgentTurn` so a
// read-only turn (the proactive daily brief) runs with zero side effects -
// loop.js/executor.js/gate.js already check `ctx.dryRun` at every write site.
import { runAgentTurn } from "./loop.js";

// Parses argv (from `process.argv.slice(2)`) into the run's inputs. Pure and
// exported so the flag contract is unit-testable without spawning the process.
// Positional args are prompt then workspaceId, in order; `--dry-run` may appear
// anywhere.
export function parseArgs(args) {
  const positional = [];
  let dryRun = false;
  for (const arg of args) {
    if (arg === "--dry-run") {
      dryRun = true;
    } else {
      positional.push(arg);
    }
  }
  return { prompt: positional[0], workspaceId: positional[1], dryRun };
}

if (process.argv[1] === import.meta.filename) {
  const { prompt, workspaceId, dryRun } = parseArgs(process.argv.slice(2));
  if (!prompt || !workspaceId) {
    console.error("usage: node agent/run.js <prompt> <workspaceId> [--dry-run]");
    process.exit(2);
  }
  const result = await runAgentTurn(prompt, workspaceId, { dryRun });
  console.log(JSON.stringify(result));
  process.exitCode = result.success ? 0 : 1;
}
