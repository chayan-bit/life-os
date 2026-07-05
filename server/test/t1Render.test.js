// T1 validator (issue #133) unit tests - fake servers only, mirroring
// renderSmoke.test.js's pattern exactly. Never boots a real cargo/vite
// process inside vitest.
import http from "node:http";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { validateT1Render } from "../validators/t1Render.js";

let servers = [];
let tmpDirs = [];

function listen(requestHandler) {
  return new Promise((resolve) => {
    const server = http.createServer(requestHandler);
    servers.push(server);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      resolve({ url: `http://127.0.0.1:${port}`, server });
    });
  });
}

afterEach(async () => {
  await Promise.all(servers.map((s) => new Promise((resolve) => s.close(resolve))));
  servers = [];
  await Promise.all(tmpDirs.map((d) => fs.rm(d, { recursive: true, force: true })));
  tmpDirs = [];
});

// A minimal git-worktree-shaped scratch dir with the three files a T1 build
// must produce/touch, so checkRegistration() finds what it expects.
async function writeRegisteredWorktree(kind, { registerInPage = true, registerInKinds = true } = {}) {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), "lifeos-t1-worktree-"));
  tmpDirs.push(dir);
  const pascal = kind.charAt(0).toUpperCase() + kind.slice(1);
  const renderersDir = path.join(dir, "frontend/src/core/renderers");
  await fs.mkdir(renderersDir, { recursive: true });
  await fs.writeFile(path.join(renderersDir, `Generic${pascal}.jsx`), "export default function C() { return null; }\n", "utf8");
  await fs.writeFile(
    path.join(dir, "frontend/src/core/ModuleManifestPage.jsx"),
    registerInPage ? `const KIND_RENDERERS = { ${kind}: Generic${pascal} };\n` : "const KIND_RENDERERS = {};\n",
    "utf8",
  );
  await fs.writeFile(
    path.join(dir, "frontend/src/core/rendererKinds.js"),
    registerInKinds ? `export const RENDERER_KINDS = ['${kind}'];\n` : "export const RENDERER_KINDS = [];\n",
    "utf8",
  );
  return dir;
}

function startFakeApi() {
  const events = [];
  function readBody(req) {
    return new Promise((resolve) => {
      let body = "";
      req.on("data", (chunk) => (body += chunk));
      req.on("end", () => resolve(body ? JSON.parse(body) : {}));
    });
  }
  return listen(async (req, res) => {
    res.setHeader("Access-Control-Allow-Origin", "*");
    if (req.url === "/api/health") {
      res.writeHead(200).end("ok");
      return;
    }
    if (req.method === "POST" && req.url === "/api/event") {
      events.push(await readBody(req));
      res.writeHead(200, { "content-type": "application/json" }).end("{}");
      return;
    }
    if (req.method === "GET" && req.url.startsWith("/api/event")) {
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(events));
      return;
    }
    res.writeHead(404).end();
  });
}

// A fake frontend mirroring renderSmoke.test.js's: polls for module.installed
// and dispatches module-mounted:<id>; the one declared view ("t1_proof_view")
// mounts a node containing (or not) a focusable child, per test case.
function startFakeFrontend(apiUrl, { throwOnLoad = false, focusableChild = true, brokenAfterMount = false } = {}) {
  const viewHtml = focusableChild
    ? '<div data-view-id="t1_proof_view"><button tabindex="0">node</button></div>'
    : '<div data-view-id="t1_proof_view"><span>no focusable child</span></div>';
  const html = `<!doctype html><html><body>
    <button data-view-tab="t1_proof_view" onclick="mountView()">tab</button>
    <div id="view-container"></div>
    <script>
      ${throwOnLoad ? "throw new Error('simulated render crash');" : ""}
      function mountView() {
        document.getElementById('view-container').innerHTML = ${JSON.stringify(viewHtml)};
        ${brokenAfterMount ? "setTimeout(() => { throw new Error('post-mount crash'); }, 10);" : ""}
      }
      setInterval(async () => {
        const res = await fetch(${JSON.stringify(apiUrl)} + "/api/event?type=module.installed");
        const events = await res.json();
        for (const ev of events) {
          window.dispatchEvent(new CustomEvent("module-mounted:" + ev.attrs.id));
        }
      }, 100);
    </script></body></html>`;
  return listen((req, res) => {
    res.writeHead(200, { "content-type": "text/html" }).end(html);
  });
}

describe("validateT1Render - registration check (no browser needed)", () => {
  it("fails when the renderer file is not registered in ModuleManifestPage.jsx", async () => {
    const worktreePath = await writeRegisteredWorktree("graph", { registerInPage: false });
    const result = await validateT1Render({ worktreePath, params: { kind: "graph" } });
    expect(result.valid).toBe(false);
    expect(result.errors.some((e) => e.includes("KIND_RENDERERS"))).toBe(true);
  });

  it("fails when the kind is not present in rendererKinds.js", async () => {
    const worktreePath = await writeRegisteredWorktree("graph", { registerInKinds: false });
    const result = await validateT1Render({ worktreePath, params: { kind: "graph" } });
    expect(result.valid).toBe(false);
    expect(result.errors.some((e) => e.includes("rendererKinds.js"))).toBe(true);
  });

  it("fails when params.kind is missing", async () => {
    const worktreePath = await writeRegisteredWorktree("graph");
    const result = await validateT1Render({ worktreePath, params: {} });
    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/requires params\.kind/);
  });
});

describe("validateT1Render - mount-pass path", () => {
  it("passes and bootstraps a baseline on first run", async () => {
    const worktreePath = await writeRegisteredWorktree("graph");
    const baselineDir = path.join(worktreePath, "baselines");
    const { url: apiUrl } = await startFakeApi();
    const { url: frontendUrl } = await startFakeFrontend(apiUrl);

    const result = await validateT1Render({
      worktreePath,
      params: { kind: "graph" },
      opts: {
        launchApi: async () => ({ url: apiUrl, stop: () => {} }),
        launchFrontend: async () => ({ url: frontendUrl, stop: () => {} }),
        baselineDir,
      },
    });

    expect(result).toEqual({ valid: true, errors: [] });
    const baseline = await fs.readFile(path.join(baselineDir, "t1-graph.png")).catch(() => null);
    expect(baseline).toBeTruthy();
  }, 20000);

  it("passes again on a second run against the just-bootstrapped baseline", async () => {
    const worktreePath = await writeRegisteredWorktree("graph");
    const baselineDir = path.join(worktreePath, "baselines");
    const { url: apiUrl } = await startFakeApi();
    const { url: frontendUrl } = await startFakeFrontend(apiUrl);
    const opts = {
      launchApi: async () => ({ url: apiUrl, stop: () => {} }),
      launchFrontend: async () => ({ url: frontendUrl, stop: () => {} }),
      baselineDir,
    };

    const first = await validateT1Render({ worktreePath, params: { kind: "graph" }, opts });
    const second = await validateT1Render({ worktreePath, params: { kind: "graph" }, opts });

    expect(first.valid).toBe(true);
    expect(second).toEqual({ valid: true, errors: [] });
  }, 20000);
});

describe("validateT1Render - console-error-failure path", () => {
  it("fails when the page throws on load", async () => {
    const worktreePath = await writeRegisteredWorktree("graph");
    const { url: apiUrl } = await startFakeApi();
    const { url: frontendUrl } = await startFakeFrontend(apiUrl, { throwOnLoad: true });

    const result = await validateT1Render({
      worktreePath,
      params: { kind: "graph" },
      opts: {
        launchApi: async () => ({ url: apiUrl, stop: () => {} }),
        launchFrontend: async () => ({ url: frontendUrl, stop: () => {} }),
        baselineDir: path.join(worktreePath, "baselines"),
      },
    });

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/console\/page errors/);
  }, 20000);
});

describe("validateT1Render - a11y-failure path", () => {
  it("fails when the mounted view has no focusable element", async () => {
    const worktreePath = await writeRegisteredWorktree("graph");
    const { url: apiUrl } = await startFakeApi();
    const { url: frontendUrl } = await startFakeFrontend(apiUrl, { focusableChild: false });

    const result = await validateT1Render({
      worktreePath,
      params: { kind: "graph" },
      opts: {
        launchApi: async () => ({ url: apiUrl, stop: () => {} }),
        launchFrontend: async () => ({ url: frontendUrl, stop: () => {} }),
        baselineDir: path.join(worktreePath, "baselines"),
      },
    });

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/focusable/);
  }, 20000);
});
