// CLI entry point for the multi-tier build pipeline (docs/SELF-EXTENSION-V2.md
// §4, §10). `lifeos-drain`'s ScaffoldJsBuilder spawns this exact process
// (`node build/run.js <prompt> <workspaceId>`) for a claimed module_requests
// row. Same stdout contract as scaffold.js: the last stdout line is
// runBuildPipeline's return value verbatim, JSON-encoded, so the Rust side has
// a stable process contract to parse. Not exercised by the vitest suite (needs
// a real ANTHROPIC_API_KEY and mutates real git state), same as scaffold.js's
// CLI tail.
import { runBuildPipeline } from "./pipeline.js";

if (process.argv[1] === import.meta.filename) {
  const prompt = process.argv[2];
  const workspaceId = process.argv[3];
  if (!prompt || !workspaceId) {
    console.error("usage: node build/run.js <prompt> <workspaceId>");
    process.exit(2);
  }
  const result = await runBuildPipeline(prompt, workspaceId);
  console.log(JSON.stringify(result));
  process.exitCode = result.success ? 0 : 1;
}
