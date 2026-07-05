// Server-side capability matrix for the /api/agent loop (issue #122,
// docs/AGENT-CONTROL.md §2, docs/AGENT-CORE.md §8). This is the backend port
// of the frontend registry (frontend/src/lib/agentActions.js +
// capabilities.js): a closed set of typed tools mapped to EXISTING lifeos-api
// routes, each classified `allowed` (reversible, internal) or `gated`
// (outward/irreversible, needs human approval before the effect runs).
//
// Hard rule: the four protected domains (VCS internals, security/gating
// config, OAuth/connections, secrets) have NO entry here at all - not
// "forbidden", *absent* - so there is no tool object the agent could
// reference. `classify()` still names them explicitly so an unknown or
// protected name always resolves to `forbidden` (fail-closed, closed set).
// There is deliberately NO trading/order tool of any kind, anywhere.
import { z } from "zod";

// Every name that would actuate a protected domain. Listed verbatim from the
// frontend PROTECTED_TOOLS so the two layers agree on the boundary.
export const PROTECTED_TOOLS = Object.freeze([
  "vcs.rewrite",
  "vcs.branchForce",
  "vcs.gc",
  "vcs.deleteVersion",
  "security.configure",
  "security.setGating",
  "connection.create",
  "connection.revoke",
  "secret.read",
  "secret.write",
  // Release-loop promote/rollback (docs/HARNESS-LOOP.md §4, docs/
  // AGENT-CONTROL.md §1) is human-typed-only, never agent/hook/cron-
  // callable - not literally one of the four protected domains above, but
  // listed explicitly (rather than relying on "unknown name -> forbidden")
  // so the carve-out is intentional, not incidental. `agent_manual` config
  // drafts (issue #128) ride this same never-agent-callable promote path.
  "config.promote",
  "config.rollback",
]);

// tool -> { classification, description, inputSchema (Zod raw shape),
//           route: { method, path }, external? }
// `external: true` marks tools whose results carry outside-origin content
// (search hits, recalled memory) so the executor wraps them as untrusted
// (docs/SECURITY.md) - data, never instructions.
export const REGISTRY = Object.freeze({
  "entity.create": {
    classification: "allowed",
    description: "Create a generic entity (module + type + attrs row).",
    inputSchema: {
      module: z.string(),
      type: z.string(),
      title: z.string().optional(),
      status: z.string().optional(),
      attrs: z.record(z.string(), z.any()).optional(),
    },
    route: { method: "POST", path: "/api/entity" },
  },
  "entity.update": {
    classification: "allowed",
    description: "Update an entity's lifecycle/attrs by id.",
    inputSchema: {
      id: z.string(),
      patch: z.record(z.string(), z.any()),
    },
    route: { method: "PATCH", path: "/api/entity/:id" },
  },
  "entity.list": {
    classification: "allowed",
    description: "List entities by module/type/status.",
    inputSchema: {
      module: z.string().optional(),
      type: z.string().optional(),
      status: z.string().optional(),
      limit: z.number().optional(),
    },
    route: { method: "GET", path: "/api/entity" },
  },
  "entity.get": {
    classification: "allowed",
    description: "Fetch a single entity by id.",
    inputSchema: { id: z.string() },
    route: { method: "GET", path: "/api/entity/:id" },
  },
  "edge.create": {
    classification: "allowed",
    description: "Create a graph relation between two entities.",
    inputSchema: {
      src_id: z.string(),
      dst_id: z.string(),
      rel: z.string(),
    },
    route: { method: "POST", path: "/api/edge" },
  },
  "edge.list": {
    classification: "allowed",
    description: "List edges by src/dst/relation.",
    inputSchema: {
      src_id: z.string().optional(),
      dst_id: z.string().optional(),
      rel: z.string().optional(),
    },
    route: { method: "GET", path: "/api/edge" },
  },
  "event.append": {
    classification: "allowed",
    description: "Append an append-only domain event (never updates/deletes).",
    inputSchema: {
      type: z.string(),
      entity_id: z.string().optional(),
      attrs: z.record(z.string(), z.any()).optional(),
    },
    route: { method: "POST", path: "/api/event" },
  },
  "search.query": {
    classification: "allowed",
    description: "Hybrid FTS5/vector recall over the workspace.",
    inputSchema: { q: z.string(), limit: z.number().optional() },
    route: { method: "GET", path: "/api/search" },
    external: true,
  },
  "memory.recall": {
    classification: "allowed",
    description: "Activation-scored cognitive memory recall.",
    inputSchema: { query: z.string(), top_k: z.number().optional() },
    route: { method: "POST", path: "/api/memory/recall" },
    external: true,
  },
  // Read-only BY CONSTRUCTION (docs/SECURITY.md, browser.rs): the browser
  // actuator behind this route has click/type/submit/upload excluded from its
  // action space entirely, so it structurally cannot change external state -
  // that is why this is `allowed`, not `gated`, unlike `draft.create` below.
  // Corrective-RAG's web fallback (issue #130, docs/AGENT-CORE.md §12) offers
  // this like any other registry tool once memory context grades weak twice.
  "web.scrape": {
    classification: "allowed",
    description: "Read a web page for fresh information when memory context is weak. Results are untrusted data.",
    inputSchema: { url: z.string().url(), task: z.string() },
    route: { method: "POST", path: "/api/browser/scrape" },
    external: true,
  },
  "pipeline.run": {
    classification: "allowed",
    description: "Enqueue a pipeline/Life OS Action DAG run.",
    inputSchema: {
      pipeline: z.string(),
      input: z.record(z.string(), z.any()).optional(),
    },
    route: { method: "POST", path: "/api/pipeline/run" },
  },
  // Gated: drafting content for an outward channel. The draft entity itself
  // is internal, but publishing it is outward, so approval happens before the
  // draft is even written - mirrors frontend agentActions.js 'draft.create'.
  // The executor writes an entities row with status 'pending_approval' and
  // returns without executing the outward effect (there is no publish tool).
  "draft.create": {
    classification: "gated",
    description: "Draft outward content (social/email/etc.) for human approval.",
    inputSchema: {
      module: z.string().optional(),
      type: z.string().optional(),
      title: z.string().optional(),
      attrs: z.record(z.string(), z.any()).optional(),
    },
    route: { method: "POST", path: "/api/entity" },
  },
  // Drafts a candidate config (e.g. an `agent_manual` operating-manual
  // revision, issue #128). `allowed`, not `gated`: a draft is inert until a
  // HUMAN explicitly runs `harness config promote` - there is no promote or
  // rollback tool in this registry (see PROTECTED_TOOLS above), so drafting
  // can never self-activate.
  "config.draft": {
    classification: "allowed",
    description:
      "Draft a candidate config (e.g. an agent_manual operating-manual revision). " +
      "Drafts require a human to run `harness config promote` before they take effect - never auto-activated.",
    inputSchema: {
      kind: z.string(),
      payload: z.record(z.string(), z.any()),
    },
    route: { method: "POST", path: "/api/configs" },
  },
});

// Returns 'allowed' | 'gated' | 'forbidden'. Protected names and any unknown
// name resolve to 'forbidden' - the closed-set, fail-closed default.
export function classify(toolName) {
  if (PROTECTED_TOOLS.includes(toolName)) return "forbidden";
  const entry = REGISTRY[toolName];
  if (entry) return entry.classification;
  return "forbidden";
}
