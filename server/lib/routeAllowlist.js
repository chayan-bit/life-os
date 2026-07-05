// Shared route allowlist for self-authored (T2) agent tools (docs/
// SELF-EXTENSION-V2.md T2 row, issue #134). A generated tool is a pure
// request descriptor - `request({args, workspaceId}) => {method, path, body?}`
// - and the executor is the only place that ever performs the HTTP call, so
// this is the single choke-check both the loader (load-time defense-in-depth)
// and the executor (call-time, the real enforcement) call, plus the T2
// validator (build-time). One list, three callers, never duplicated logic.
//
// Fail-closed: the list below is a WHITELIST. Anything not explicitly matched
// - configs, module-request, jobs, llm, agent, whatsapp, storage, travel,
// notion, connections, anything with 'order'/'broker' - is denied by omission,
// not by a growing deny-list.
const ALLOWED_ROUTES = Object.freeze([
  { method: "GET", pattern: /^\/api\/entity(\/[^/?]+)?(\?.*)?$/ },
  { method: "POST", pattern: /^\/api\/entity(\/[^/?]+)?(\?.*)?$/ },
  { method: "GET", pattern: /^\/api\/edge(\/[^/?]+)?(\?.*)?$/ },
  { method: "POST", pattern: /^\/api\/edge(\/[^/?]+)?(\?.*)?$/ },
  { method: "GET", pattern: /^\/api\/search(\?.*)?$/ },
  { method: "POST", pattern: /^\/api\/memory\/recall$/ },
  { method: "POST", pattern: /^\/api\/event$/ },
  { method: "POST", pattern: /^\/api\/browser\/scrape$/ },
]);

// True iff `method`+`path` matches one of the whitelisted generated-tool
// routes. Any non-string input, or anything outside the whitelist, is denied.
export function isRouteAllowed(method, path) {
  if (typeof method !== "string" || typeof path !== "string" || path.length === 0) return false;
  const upperMethod = method.toUpperCase();
  return ALLOWED_ROUTES.some((route) => route.method === upperMethod && route.pattern.test(path));
}

export { ALLOWED_ROUTES };
