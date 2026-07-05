// T1 validator (issue #133, docs/SELF-EXTENSION-V2.md §9 T1 row) - the first
// real (non-placeholder) tier-specific validator. Reuses render.js's own
// boot pattern (launchApi/launchFrontend against a scratch DB on ephemeral
// ports, a real Playwright browser, the app's real module-mounted:<id>
// ready event) rather than reimplementing it, adds a static
// files-exist-and-are-registered check, an a11y lint, and a manual
// (no-new-dependency) visual baseline compare.
//
// Scope: this validator only proves the T1 build produced a working,
// registered, keyboard-accessible renderer - it is not a general-purpose
// render-smoke (that's render.js, reused as-is for T0's full multi-view
// manifest check).
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { chromium } from "playwright";
import { getEphemeralPort, launchApi as defaultLaunchApi, launchFrontend as defaultLaunchFrontend } from "../lib/appBoot.js";

const MOUNT_TIMEOUT_MS = 10000;
const DEFAULT_BASELINE_DIR = path.resolve(import.meta.dirname, "baselines");
const VIEW_ID = "t1_proof_view";

// Known limitation (honesty over a fake-passing comparison, per the T1
// design note): this repo takes zero new npm dependencies, so there is no
// pixelmatch-style perceptual diff available. The baseline gate below is a
// coarse dimension + byte-size-ratio comparison - it catches a blank/broken
// render or a wildly different layout, but a subtle color or spacing
// regression within the same byte budget will not be caught. Documented in
// docs/SELF-EXTENSION-V2.md's "Implemented (issue #133)" note as a known
// follow-up (a real perceptual diff library, if one is ever added).
const BYTE_RATIO_TOLERANCE = 0.35;

function pascalKind(kind) {
  if (typeof kind !== "string" || kind.length === 0) return "";
  return kind.charAt(0).toUpperCase() + kind.slice(1);
}

// (a) Static check: the renderer file exists on disk AND is wired into both
// registries a T1 build must touch - KIND_RENDERERS (ModuleManifestPage.jsx)
// and RENDERER_KINDS (rendererKinds.js). A build that wrote the component
// but forgot either registration still fails closed here, before any
// browser is ever launched.
async function checkRegistration(worktreePath, kind) {
  const errors = [];
  const rendererRel = `frontend/src/core/renderers/Generic${pascalKind(kind)}.jsx`;

  try {
    await fs.access(path.join(worktreePath, rendererRel));
  } catch {
    errors.push(`${rendererRel} does not exist`);
  }

  const pageSource = await fs
    .readFile(path.join(worktreePath, "frontend/src/core/ModuleManifestPage.jsx"), "utf8")
    .catch(() => null);
  if (pageSource === null) {
    errors.push("frontend/src/core/ModuleManifestPage.jsx does not exist");
  } else if (!new RegExp(`\\b${kind}\\s*:\\s*Generic${pascalKind(kind)}\\b`).test(pageSource)) {
    errors.push(`kind '${kind}' is not registered in ModuleManifestPage.jsx's KIND_RENDERERS map`);
  }

  const kindsSource = await fs
    .readFile(path.join(worktreePath, "frontend/src/core/rendererKinds.js"), "utf8")
    .catch(() => null);
  if (kindsSource === null) {
    errors.push("frontend/src/core/rendererKinds.js does not exist");
  } else if (!new RegExp(`['"]${kind}['"]`).test(kindsSource)) {
    errors.push(`kind '${kind}' is missing from rendererKinds.js's RENDERER_KINDS array`);
  }

  return errors;
}

// (d) Visual baseline: bootstrap (save + pass) on first run, manual
// dimension-free byte-ratio compare against an existing baseline otherwise.
async function compareOrBootstrapBaseline(kind, screenshotBuffer, baselineDir) {
  const baselinePath = path.join(baselineDir, `t1-${kind}.png`);
  let existing;
  try {
    existing = await fs.readFile(baselinePath);
  } catch {
    await fs.mkdir(baselineDir, { recursive: true });
    await fs.writeFile(baselinePath, screenshotBuffer);
    return { valid: true, errors: [] };
  }
  const ratio = Math.abs(screenshotBuffer.length - existing.length) / Math.max(existing.length, 1);
  if (ratio > BYTE_RATIO_TOLERANCE) {
    return {
      valid: false,
      errors: [
        `screenshot for '${kind}' diverges from baseline t1-${kind}.png by ${(ratio * 100).toFixed(1)}% ` +
          `(tolerance ${(BYTE_RATIO_TOLERANCE * 100).toFixed(0)}%) - re-run with a clean baseline if this is an intended visual change`,
      ],
    };
  }
  return { valid: true, errors: [] };
}

// (c) a11y lint: at least one keyboard-focusable element exists inside the
// mounted view, and a real `.focus()` call actually lands on it (not merely
// present in markup with a broken tabindex/disabled state).
async function checkFocusable(page) {
  const selector =
    `[data-view-id="${VIEW_ID}"] [tabindex], ` +
    `[data-view-id="${VIEW_ID}"] button, ` +
    `[data-view-id="${VIEW_ID}"] a[href], ` +
    `[data-view-id="${VIEW_ID}"] input`;
  const count = await page.locator(selector).count();
  if (count === 0) {
    throw new Error(`no keyboard-focusable element found inside [data-view-id="${VIEW_ID}"]`);
  }
  const focused = await page.evaluate(
    ({ sel }) => {
      const el = document.querySelector(sel);
      if (!el) return false;
      el.focus();
      return document.activeElement === el;
    },
    { sel: selector },
  );
  if (!focused) {
    throw new Error(`focusable element inside [data-view-id="${VIEW_ID}"] did not accept focus()`);
  }
}

async function runOnce({ worktreePath, kind, opts }) {
  const repoRoot = worktreePath;
  const moduleId = `t1_${kind}`;
  const launchApi = opts.launchApi ?? defaultLaunchApi;
  const launchFrontend = opts.launchFrontend ?? defaultLaunchFrontend;
  const openBrowser = opts.openBrowser ?? (() => chromium.launch());
  const baselineDir = opts.baselineDir ?? DEFAULT_BASELINE_DIR;
  const dbDir = await fs.mkdtemp(path.join(os.tmpdir(), "lifeos-t1-render-"));
  const apiPort = await getEphemeralPort();
  const frontendPort = await getEphemeralPort();

  let api;
  let frontend;
  let browser;
  let context;
  let page;
  const jsErrors = [];

  try {
    api = await launchApi({ repoRoot, dbDir, port: apiPort });
    frontend = await launchFrontend({ repoRoot, apiUrl: api.url, port: frontendPort });

    browser = await openBrowser();
    context = await browser.newContext();
    await context.addInitScript(() => {
      window.localStorage.setItem("life_os_loggedin", "true");
    });

    let onFirstError;
    const firstError = new Promise((resolve) => {
      onFirstError = resolve;
    });
    page = await context.newPage();
    page.on("console", (msg) => {
      if (msg.type() === "error") {
        jsErrors.push(msg.text());
        onFirstError();
      }
    });
    page.on("pageerror", (err) => {
      jsErrors.push(err.message);
      onFirstError();
    });

    await page.goto(frontend.url, { waitUntil: "load" }).catch(() => {});
    if (jsErrors.length > 0) {
      throw new Error(`console/page errors during render: ${jsErrors.join("; ")}`);
    }

    const mounted = page.evaluate(
      (id) =>
        new Promise((resolve) => {
          window.addEventListener(`module-mounted:${id}`, () => resolve(), { once: true });
        }),
      moduleId,
    );

    const eventRes = await fetch(`${api.url}/api/event`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ type: "module.installed", attrs: { id: moduleId, name: moduleId, view: VIEW_ID } }),
    });
    if (!eventRes.ok) {
      throw new Error(`failed to seed module.installed event: HTTP ${eventRes.status}`);
    }

    const winner = await Promise.race([
      mounted.then(() => "mounted"),
      firstError.then(() => "error"),
      new Promise((resolve) => setTimeout(() => resolve("timeout"), MOUNT_TIMEOUT_MS)),
    ]);
    if (winner === "error" || jsErrors.length > 0) {
      throw new Error(`console/page errors during render: ${jsErrors.join("; ")}`);
    }
    if (winner === "timeout") {
      throw new Error(`module-mounted:${moduleId} did not fire within ${MOUNT_TIMEOUT_MS}ms`);
    }

    await page.click(`[data-view-tab="${VIEW_ID}"]`, { timeout: MOUNT_TIMEOUT_MS });
    await page.waitForSelector(`[data-view-id="${VIEW_ID}"]`, { timeout: MOUNT_TIMEOUT_MS, state: "attached" });
    if (jsErrors.length > 0) {
      throw new Error(`console/page errors during render: ${jsErrors.join("; ")}`);
    }

    await checkFocusable(page);

    const screenshot = await page.screenshot();
    const baselineResult = await compareOrBootstrapBaseline(kind, screenshot, baselineDir);
    if (!baselineResult.valid) {
      throw new Error(baselineResult.errors.join("; "));
    }

    return { valid: true, errors: [] };
  } catch (error) {
    return { valid: false, errors: [error.message] };
  } finally {
    await page?.close().catch(() => {});
    await context?.close().catch(() => {});
    await browser?.close().catch(() => {});
    frontend?.stop();
    api?.stop();
    await fs.rm(dbDir, { recursive: true, force: true }).catch(() => {});
  }
}

// validateT1Render({ worktreePath, params: { kind }, opts }) - opts carries
// the same launchApi/launchFrontend/openBrowser DI seams as render.js
// (plus baselineDir), so tests never boot a real cargo/vite process.
export async function validateT1Render({ worktreePath, params = {}, opts = {} } = {}) {
  const kind = params.kind;
  if (!kind) {
    return { valid: false, errors: ["T1 validator requires params.kind"] };
  }

  const registrationErrors = await checkRegistration(worktreePath, kind);
  if (registrationErrors.length > 0) {
    return { valid: false, errors: registrationErrors };
  }

  return runOnce({ worktreePath, kind, opts });
}

export const t1RenderValidator = { name: "t1Render", run: validateT1Render };
