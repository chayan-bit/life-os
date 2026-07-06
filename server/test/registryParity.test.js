import { describe, expect, it } from "vitest";
import {
  PROTECTED_TOOLS as BACKEND_PROTECTED,
  REGISTRY as BACKEND_REGISTRY,
  ROLE_CAPS as BACKEND_ROLE_CAPS,
} from "../agent/actionRegistry.js";
import {
  PROTECTED_TOOLS as FRONTEND_PROTECTED,
  ACTION_TOOLS as FRONTEND_TOOLS,
  ROLE_CAPS as FRONTEND_ROLE_CAPS,
} from "../../frontend/src/lib/agentActions.js";

// Guards the split maintained between the two independently-maintained agent
// tool registries: server/agent/actionRegistry.js (backend, enforced) and
// frontend/src/lib/agentActions.js (browser AI console, client-side). They
// were found to disagree on pipeline.run's classification - this test fails
// the build the moment one side changes a shared tool's classification (or
// the protected-tool boundary) without the other side following.
describe("agent registry parity - backend actionRegistry.js vs frontend agentActions.js", () => {
  it("agrees on classification for every tool name present in both registries", () => {
    const sharedNames = Object.keys(FRONTEND_TOOLS).filter((name) => name in BACKEND_REGISTRY);
    // Sanity: there must be real overlap, otherwise this test would pass
    // vacuously and never catch a real drift.
    expect(sharedNames.length).toBeGreaterThan(0);

    for (const name of sharedNames) {
      const backendClassification = BACKEND_REGISTRY[name].classification;
      const frontendClassification = FRONTEND_TOOLS[name].classification;
      expect(
        frontendClassification,
        `tool '${name}' is '${frontendClassification}' in agentActions.js but '${backendClassification}' in actionRegistry.js`
      ).toBe(backendClassification);
    }
  });

  it("agrees on the PROTECTED_TOOLS intersection", () => {
    const backendSet = new Set(BACKEND_PROTECTED);
    const frontendSet = new Set(FRONTEND_PROTECTED);
    const intersection = [...backendSet].filter((name) => frontendSet.has(name));
    expect(intersection.length).toBeGreaterThan(0);

    for (const name of intersection) {
      expect(backendSet.has(name)).toBe(true);
      expect(frontendSet.has(name)).toBe(true);
    }

    // Every backend-protected name that also appears in the frontend registry
    // as a callable tool would be a hard boundary violation - protected names
    // must never resolve to a runnable frontend tool object.
    for (const name of backendSet) {
      expect(FRONTEND_TOOLS[name]).toBeUndefined();
    }
    for (const name of frontendSet) {
      expect(BACKEND_REGISTRY[name]).toBeUndefined();
    }
  });

  // Issue #146: the per-role capability matrix is exported from both registries
  // and MUST stay symmetric - the executor (backend) and the Profile matrix
  // (frontend) both read it, so any drift would let one layer grant a reach the
  // other denies.
  it("agrees on the ROLE_CAPS matrix byte-for-byte across both registries", () => {
    expect(FRONTEND_ROLE_CAPS).toEqual(BACKEND_ROLE_CAPS);

    // And it stays a closed, sane set: known roles only, no role can execute a
    // 'forbidden' classification, viewer executes nothing.
    const roles = Object.keys(BACKEND_ROLE_CAPS).sort();
    expect(roles).toEqual(["agent", "editor", "owner", "viewer"]);
    for (const [role, caps] of Object.entries(BACKEND_ROLE_CAPS)) {
      expect(caps).not.toContain("forbidden");
      for (const cap of caps) expect(["allowed", "gated"]).toContain(cap);
    }
    expect(BACKEND_ROLE_CAPS.viewer).toEqual([]);
  });
});
