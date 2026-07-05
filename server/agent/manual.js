// Operating manual injection (docs/AGENT-CORE.md §6, docs/HARNESS-LOOP.md §4,
// issue #128). The manual is a single versioned `config`
// (`kind='agent_manual'`) reusing the release-loop `configs` + `vcs_refs
// (kind='config_active')` machinery `POST /api/configs` /
// `GET /api/configs` already implement (services/lifeos-api/src/routes/
// configs.rs) - no new subsystem. Promoting a draft live is human-typed
// only (`harness config promote`); this module only ever reads the already-
// active one.
const MANUAL_KIND = "agent_manual";
const MANUAL_BLOCK_LABEL = "## Operating manual";

// fetchActiveManual(httpFn, workspaceId) - returns a labeled text block for
// the currently-active `agent_manual` config, or null on absence/error.
// Failure-tolerant like fetchMemoryContext: a manual outage (or no manual
// ever promoted) must never fail, or pad, a turn.
export async function fetchActiveManual(httpFn, workspaceId) {
  try {
    const qs = new URLSearchParams({ kind: MANUAL_KIND, workspace_id: workspaceId }).toString();
    const res = await httpFn("GET", `/api/configs?${qs}`);
    if (!res?.ok) return null;

    const activeId = res.data?.active?.[MANUAL_KIND];
    if (!activeId) return null;

    const configs = res.data?.configs ?? [];
    const active = configs.find((c) => c.id === activeId);
    if (!active?.payload) return null;

    const payloadText = typeof active.payload === "string" ? active.payload : JSON.stringify(active.payload);
    return [MANUAL_BLOCK_LABEL, payloadText].join("\n");
  } catch {
    return null;
  }
}
