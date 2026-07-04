// Validator 2 - render smoke, headless Playwright (issue #75,
// docs/SELF-EXTENSION.md §4). Replaces the earlier fake stub (unconditional
// `return true`, a `// In a real environment we would...` comment) with a
// real boot of the app stack against a scratch DB on ephemeral ports.
//
// Scope note (issue #121, docs/SELF-EXTENSION-V2.md §6): the hot-install
// path now persists the real object-shaped manifest (module.js's own
// osRegisterModule({...}) argument) as a `module='system'`,
// `type='module_manifest'` entity (server/lib/manifestEntity.js), and
// InstalledModulePage.jsx mounts the full multi-view ModuleManifestPage for
// it instead of degrading to a flat GenericList. So this validator now
// asserts, end-to-end against the real app: 0 console/page errors, the
// `module-mounted:<id>` ready event fires, AND - when `opts.modulePath` is
// given - every view the manifest declares actually mounts a DOM node
// (`[data-view-tab="<id>"]` / `[data-view-id="<id>"]`, ModuleManifestPage.jsx).
// The previously-flagged scope gap ("no live per-view render path for a
// hot-installed module to assert against") is closed.
import { chromium } from "playwright";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { getEphemeralPort, launchApi as defaultLaunchApi, launchFrontend as defaultLaunchFrontend } from "../lib/appBoot.js";
import { loadManifestFromFile } from "../lib/loadManifest.js";
import { persistManifestEntity as defaultPersistManifestEntity } from "../lib/manifestEntity.js";

const DEFAULT_REPO_ROOT = path.resolve(import.meta.dirname, "..", "..");
const MOUNT_TIMEOUT_MS = 10000;
const MAX_ATTEMPTS = 2; // one bounded retry, per #75's checklist

async function runOnce(moduleId, manifest, opts) {
  const repoRoot = opts.repoRoot ?? DEFAULT_REPO_ROOT;
  const launchApi = opts.launchApi ?? defaultLaunchApi;
  const launchFrontend = opts.launchFrontend ?? defaultLaunchFrontend;
  const openBrowser = opts.openBrowser ?? (() => chromium.launch());
  const persistManifestEntity = opts.persistManifestEntity ?? defaultPersistManifestEntity;
  const dbDir = await fs.mkdtemp(path.join(os.tmpdir(), "lifeos-render-smoke-"));
  const apiPort = await getEphemeralPort();
  const frontendPort = await getEphemeralPort();

  let api;
  let frontend;
  let browser;
  let context;
  let page;
  const jsErrors = [];

  try {
    // `opts.modulePath` points at the real module.js still sitting in the
    // scaffold worktree (scaffold.js calls this before removeWorktree) - its
    // object-shaped entityTypes/views is what ModuleManifestPage.jsx actually
    // renders, unlike the array-shaped structured-output `manifest` param.
    // Falls back to `manifest` itself (whatever shape/fields the caller
    // passed) when no modulePath is given, e.g. this file's own unit tests.
    const fullManifest = opts.modulePath ? await loadManifestFromFile(opts.modulePath) : manifest;
    const views = fullManifest?.views ?? [];

    api = await launchApi({ repoRoot, dbDir, port: apiPort });
    frontend = await launchFrontend({ repoRoot, apiUrl: api.url, port: frontendPort });

    // Seeds the manifest entity the frontend fetches on `module.installed`
    // (frontend/src/lib/manifestApi.js's fetchInstalledManifest) - same
    // entity write route/upsert-by-title convention as scaffold.js's real
    // install path (server/lib/manifestEntity.js), so this validator
    // exercises the exact mechanism a live install uses.
    await persistManifestEntity(api.url, moduleId, fullManifest);

    browser = await openBrowser();
    context = await browser.newContext();
    // Bypasses the SPA's client-side login gate (App.jsx checks
    // localStorage) - this validator is testing module rendering, not auth.
    await context.addInitScript(() => {
      window.localStorage.setItem("life_os_loggedin", "true");
    });
    // Resolves the instant the first console/page error is observed, so a
    // page that crashes on load fails fast with an accurate message instead
    // of waiting out the full mount timeout below.
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

    await page.goto(frontend.url, { waitUntil: "load" }).catch(() => {
      // A page that throws synchronously during initial script execution can
      // abort navigation in some engines - pageerror above still captured
      // the error, so let the firstError race below report it.
    });

    if (jsErrors.length > 0) {
      throw new Error(`console/page errors during render: ${jsErrors.join("; ")}`);
    }

    // Waits on the real CustomEvent moduleRegistry.js dispatches, not an
    // arbitrary timeout - the timeout below is only a safety net so a
    // never-firing event fails the build instead of hanging it.
    const mounted = page.evaluate(
      (id) =>
        new Promise((resolve) => {
          window.addEventListener(`module-mounted:${id}`, () => resolve(), { once: true });
        }),
      moduleId,
    );

    // Seeds the exact event the real self-extension install path emits
    // (docs/SELF-EXTENSION.md §1 step 5) - no auth needed, /api/event falls
    // back to the default workspace when no bearer token is presented
    // (src/auth.rs resolve_workspace), which is what the frontend does too.
    const eventRes = await fetch(`${api.url}/api/event`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ type: "module.installed", attrs: { id: moduleId, name: manifest?.name ?? moduleId } }),
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

    // Per-view assertion (issue #121): every declared view must actually
    // mount a node when its tab is clicked - closes the scope gap this
    // file's header used to flag. A module with no views (shouldn't happen
    // post-structural-validation, docs/SELF-EXTENSION.md §4) skips the loop.
    for (const declaredView of views) {
      await page.click(`[data-view-tab="${declaredView.id}"]`, { timeout: MOUNT_TIMEOUT_MS });
      // `state: "attached"` (not the default "visible") - the assertion is
      // "this view's node exists in the DOM", not that it has a non-empty
      // layout box, which an otherwise-correct empty view container can lack.
      await page.waitForSelector(`[data-view-id="${declaredView.id}"]`, { timeout: MOUNT_TIMEOUT_MS, state: "attached" });
      if (jsErrors.length > 0) {
        throw new Error(`console/page errors during render: ${jsErrors.join("; ")}`);
      }
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

export async function validateRenderSmoke(moduleId, manifest, opts = {}) {
  let result;
  for (let attempt = 1; attempt <= MAX_ATTEMPTS; attempt++) {
    result = await runOnce(moduleId, manifest, opts);
    if (result.valid) return result;
  }
  return result;
}
