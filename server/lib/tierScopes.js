// Per-tier sandbox scope + never-generable-surface enforcement - the safety
// spine of the self-extension ladder (docs/SELF-EXTENSION-V2.md §3, §5, §9).
//
// This is the single source of truth for "what a build at tier T may write."
// It generalizes the hardcoded `modules/<id>/` confinement of Tier 0 into a
// per-tier allowlist of path globs, and hard-excludes the four never-generable
// domains (AGENT-CONTROL.md §1) at EVERY tier, deny-wins-over-allow.
//
// Fail-closed everywhere: an unknown tier, a path that resolves outside the
// sandbox root, or any match against a protected surface all DENY.
import path from "node:path";
import picomatch from "picomatch";

const PICO_OPTS = { dot: true }; // dot:true so `.git`/`.github`/`.claude` match

// Renderer files are PascalCase (`GenericBoard.jsx`); a T1 build declares a
// view `kind` (`board`, `graph`, ...) and may only write that one renderer.
function pascalKind(kind) {
  if (typeof kind !== "string" || kind.length === 0) return "";
  return kind.charAt(0).toUpperCase() + kind.slice(1);
}

// Per-tier write-scope: a function of build params -> repo-relative globs.
// Each tier's scope is deliberately the TIGHTEST set of paths that tier needs,
// not the broadest the doc allows - the whitelist is the security boundary.
export const TIER_SCOPES = {
  // T0 Manifest - one osRegisterModule({...}) file (built today).
  T0: ({ moduleId }) => [`modules/${moduleId}/**`],

  // T1 View / renderer - the single new Generic<Kind>.jsx, its registration
  // in the KIND_RENDERERS map (ModuleManifestPage.jsx), and rendererKinds.js
  // (the plain-JS RENDERER_KINDS array structural.js derives its known-kinds
  // set from, issue #133) - the one deliberate scope-file addition beyond
  // the original two, since a new kind is not real until both the
  // component map AND the kind registry know about it. Scoped to the one
  // renderer, not `renderers/**`, so an existing renderer can't be
  // overwritten.
  T1: ({ kind }) => [
    `frontend/src/core/renderers/Generic${pascalKind(kind)}.jsx`,
    "frontend/src/core/ModuleManifestPage.jsx",
    "frontend/src/core/rendererKinds.js",
  ],

  // T2 Capability (self-authored agent tool, issue #134) - ONE pure request
  // descriptor file under the generated-tools dir; the loader
  // (server/agent/tools/generated/index.js) auto-discovers it, so a T2 build
  // never touches index.js itself.
  T2: ({ name }) => [`server/agent/tools/generated/${name}.js`],

  // T3 Backend route (issue #135) - the one named route file, its additive
  // registration in the crate's routes/mod.rs, and its own integration test.
  // Scoped to the three concrete files (not `routes/**`), so a build can
  // never overwrite a sibling route or any other file in the crate; mod.rs is
  // in scope only because registration is unavoidable - protectedSurface
  // still independently blocks every protected surface, and the T3 validator
  // (server/validators/t3Route.js) diff-checks mod.rs is edited additively.
  T3: ({ crate, name }) => [
    `services/${crate}/src/routes/${name}.rs`,
    `services/${crate}/src/routes/mod.rs`,
    `services/${crate}/tests/${name}_integration.rs`,
  ],

  // T4 Migration / derived - an additive migration file or a derived-index
  // rebuild. Protected migrations (§5) are still denied even though the scope
  // names `migrations/**`, because deny wins.
  T4: () => ["migrations/**", "services/lifeos-derived/**"],

  // T5 Subsystem - a whole new crate dir plus its Cargo.toml workspace entry.
  T5: ({ crate }) => [`services/${crate}/**`, "services/Cargo.toml"],
};

// The four never-generable domains (docs/SELF-EXTENSION-V2.md §5,
// AGENT-CONTROL.md §1), matched against EVERY write at EVERY tier; deny wins
// over any tier allow. Includes self-protection (the enforcement code and
// validators cannot be rewritten by the thing they constrain).
export const PROTECTED_SURFACES = [
  // -- Self-protection: the sandbox + enforcement + validators themselves.
  "server/lib/sandbox.js",
  "server/lib/preToolUseHook.js",
  "server/lib/tierScopes.js",
  "server/validators/**",
  // -- Security & gating config: capability matrix + typed action registry.
  "server/agent/actionRegistry.js",
  "frontend/src/lib/capabilities.js",
  "frontend/src/lib/capabilityMatrix.js",
  "frontend/src/lib/agentActions.js",
  "frontend/src/lib/actionPlanCompiler.js",
  // -- broker-guard + any order-execution path: trading is read-only for any
  //    agent; no order tool/route may ever be generated.
  "**/broker-guard*",
  "**/broker-guard*/**",
  "services/broker-guard/**",
  "**/*order*.rs",
  "**/orders/**",
  // -- OAuth / connections / secrets (Nango vault + envelope-encrypted cols).
  "infra/nango/**",
  "migrations/0002_control_plane.sql",
  "migrations/0011_workspace_envelope_key.sql",
  // -- VCS internals: history is the audit source of truth (forward-only).
  "services/lifeos-vcs/**",
  // -- Meta: git state, CI workflows, harness config.
  ".git/**",
  ".github/**",
  ".claude/**",
];

const protectedMatcher = picomatch(PROTECTED_SURFACES, PICO_OPTS);

// Resolves `filePath` (absolute or relative to `root`) to a POSIX repo-relative
// path, or returns null when it escapes `root` (path traversal / an absolute
// path elsewhere). Layer B (this function) blocks `..`, absolute-outside, and
// symlink paths whose lexical resolution escapes; Layer C (Seatbelt) is the
// kernel backstop for symlink races Node cannot resolve statically.
export function toRepoRelative(root, filePath) {
  if (typeof filePath !== "string" || filePath.length === 0) return null;
  const resolvedRoot = path.resolve(root);
  const resolvedFile = path.resolve(resolvedRoot, filePath);
  const rel = path.relative(resolvedRoot, resolvedFile);
  if (rel === "" || rel.startsWith("..") || path.isAbsolute(rel)) return null;
  return rel.split(path.sep).join("/");
}

// True iff a repo-relative POSIX path matches any never-generable surface.
export function isProtectedPath(relPath) {
  return typeof relPath === "string" && protectedMatcher(relPath);
}

// Boolean convenience wrapper (deny-wins): allowed only if inside root, not a
// protected surface, and inside the tier's write-scope.
export function isWriteAllowed(tier, params, filePath, root) {
  return evaluateWrite({ tier, params, root }, filePath).allowed;
}

// The single decision function Layer B calls. Returns { allowed, reason },
// where `reason` names which rule fired so a denial is diagnosable.
export function evaluateWrite(scope, filePath) {
  const { tier, params = {}, root } = scope;
  const rel = toRepoRelative(root, filePath);
  if (rel === null) return { allowed: false, reason: `path resolves outside the sandbox root: ${filePath}` };
  if (protectedMatcher(rel)) return { allowed: false, reason: `never-generable protected surface: ${rel}` };

  const scopeFn = TIER_SCOPES[tier];
  if (typeof scopeFn !== "function") return { allowed: false, reason: `unknown tier: ${tier}` };

  const globs = scopeFn(params);
  if (picomatch(globs, PICO_OPTS)(rel)) return { allowed: true, reason: null };
  return { allowed: false, reason: `outside ${tier} write-scope: ${rel}` };
}

// Top-level writable directories for Seatbelt's `filesystem.allowWrite` (Layer
// C takes directories, not globs). Derives the static prefix of each tier glob
// via picomatch.scan and collapses concrete files to their parent dir.
export function scopeDirs(tier, params = {}) {
  const scopeFn = TIER_SCOPES[tier];
  if (typeof scopeFn !== "function") return [];
  const dirs = new Set();
  for (const glob of scopeFn(params)) {
    const { base, isGlob } = picomatch.scan(glob);
    const dir = isGlob ? base : path.posix.dirname(base);
    dirs.add(`./${dir}`);
  }
  return [...dirs];
}
