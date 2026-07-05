// COMMIT stage + build event log (docs/SELF-EXTENSION-V2.md §4 step 6, §8).
// Each passing node becomes ONE conventional-commit merge to main, reusing the
// exact add -> commit -> ff-merge mechanics scaffold.js already trusts
// (lib/worktree.js::commitAndMerge, extended additively with a per-node message
// and add-scope). Every stage emits an append-only `events` row so the whole
// build is visible in `harness observe` and replayable from its events, with
// the same tier stamp the agent loop and pipeline runner use.
import { execFile as execFileCb } from "node:child_process";
import { promisify } from "node:util";
import { commitAndMerge } from "../lib/worktree.js";

const execFile = promisify(execFileCb);

// Emits one build event via the same POST /api/event path the agent loop uses
// (server/agent/loop.js::escalate). Best-effort: a trace-write failure must
// never fail or reverse the build. `tier`/`outcome` are stamped as top-level
// run-log lens fields (mirroring persistTurn), the rest ride in attrs.
export async function emitBuildEvent(ctx, type, { tier = null, node = null, outcome, ...rest }) {
  try {
    await ctx.httpFn("POST", "/api/event", {
      type,
      actor: "build",
      run_id: ctx.runId,
      tier,
      outcome,
      attrs: { run_id: ctx.runId, tier, node, outcome, ...rest },
      workspace_id: ctx.workspaceId,
    });
  } catch {
    // Best-effort trace only.
  }
}

// Resolves a checkout's current commit short hash, so the pipeline result can
// report a revertable ref per installed node (T1-T5: the worktree; T0: the repo
// root, where scaffoldModule already merged its own commit onto main).
export async function headHash(checkoutDir) {
  const { stdout } = await execFile("git", ["rev-parse", "--short", "HEAD"], { cwd: checkoutDir });
  return stdout.trim();
}

// Commits a passing T1-T5 node's worktree and fast-forwards main onto it. The
// node's writes are already confined to its tier scope by Layer B, so staging
// everything in the throwaway worktree is safe and captures the whole change.
// Returns the merged commit's short hash.
export async function commitNode(ctx, node, { worktreePath, branch }) {
  const message = `feat: ${node.tier} ${node.description} (build:${ctx.runId})`;
  await commitAndMerge(ctx.repoRoot, worktreePath, branch, null, { addPaths: ["-A"], message });
  return headHash(ctx.repoRoot);
}
