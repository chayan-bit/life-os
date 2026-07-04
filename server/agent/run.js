// CLI entry point (issue #122): `lifeos-api`'s /api/agent route spawns this
// exact process (`node agent/run.js <prompt> <workspaceId>`) from the server/
// dir, the same process contract `scaffold.js` has with lifeos-drain. The LAST
// non-empty stdout line is `runAgentTurn`'s return value, JSON-encoded, so the
// Rust side has one stable line to parse.
import { runAgentTurn } from "./loop.js";

if (process.argv[1] === import.meta.filename) {
  const prompt = process.argv[2];
  const workspaceId = process.argv[3];
  if (!prompt || !workspaceId) {
    console.error("usage: node agent/run.js <prompt> <workspaceId>");
    process.exit(2);
  }
  const result = await runAgentTurn(prompt, workspaceId);
  console.log(JSON.stringify(result));
  process.exitCode = result.success ? 0 : 1;
}
