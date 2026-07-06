// CLI entry point for the multi-tier build pipeline (docs/SELF-EXTENSION-V2.md
// §4, §10). `lifeos-drain`'s ScaffoldJsBuilder spawns this exact process
// (`node build/run.js <prompt> <workspaceId>`) for a claimed module_requests
// row. Same stdout contract as scaffold.js: the last stdout line is
// runBuildPipeline's return value verbatim, JSON-encoded, so the Rust side has
// a stable process contract to parse. Not exercised by the vitest suite (needs
// a real ANTHROPIC_API_KEY and mutates real git state), same as scaffold.js's
// CLI tail.
//
// Resume mode (issue #142): `node build/run.js --resume <approvalEntityId>
// <workspaceId>` re-enters the pipeline for an approved build gate. Same
// last-line-JSON contract, so lifeos-drain's ScaffoldJsResumer parses it the
// same way as a fresh build.
import { runBuildPipeline } from "./pipeline.js";
import { resumeBuildPipeline } from "./resume.js";

if (process.argv[1] === import.meta.filename) {
  const [, , first, second, third] = process.argv;
  if (first === "--resume") {
    const approvalEntityId = second;
    const workspaceId = third;
    if (!approvalEntityId || !workspaceId) {
      console.error("usage: node build/run.js --resume <approvalEntityId> <workspaceId>");
      process.exit(2);
    }
    const result = await resumeBuildPipeline(approvalEntityId, workspaceId);
    console.log(JSON.stringify(result));
    process.exitCode = result.success ? 0 : 1;
  } else {
    const prompt = first;
    const workspaceId = second;
    if (!prompt || !workspaceId) {
      console.error("usage: node build/run.js <prompt> <workspaceId>");
      process.exit(2);
    }
    const result = await runBuildPipeline(prompt, workspaceId);
    console.log(JSON.stringify(result));
    process.exitCode = result.success ? 0 : 1;
  }
}
