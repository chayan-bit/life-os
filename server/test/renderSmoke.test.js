// Exercises validateRenderSmoke's orchestration (retry, timeout, error
// aggregation, teardown) against real Playwright + real HTTP servers, but
// fake `launchApi`/`launchFrontend` implementations - spinning up the real
// cargo binary + Vite dev server per test would make this suite slow and
// dependent on a local build being present. server/scripts/renderSmokeLive.js
// (manual, not run by vitest) exercises the real stack end-to-end instead.
import http from "node:http";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { validateRenderSmoke } from "../validators/render.js";

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

// Writes a real module.js (a plain osRegisterModule({...}) call, same shape
// loadManifestFromFile expects) so tests can exercise render.js's
// `opts.modulePath` per-view path (issue #121) without a full scaffold worktree.
async function writeModuleFile(manifest) {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), "lifeos-render-smoke-module-"));
  tmpDirs.push(dir);
  const modulePath = path.join(dir, "module.js");
  await fs.writeFile(modulePath, `osRegisterModule(${JSON.stringify(manifest)});`, "utf8");
  return modulePath;
}

// A fake lifeos-api: /api/health always 200s; POST/GET /api/event mirror the
// event log; POST/GET/PATCH /api/entity mirror just enough of the real
// generic entity route (services/lifeos-api/src/routes/entity.rs) for
// server/lib/manifestEntity.js's upsert-by-title flow to work against it.
function startFakeApi() {
  const events = [];
  let entities = [];
  let nextId = 1;

  function readBody(req) {
    return new Promise((resolve) => {
      let body = "";
      req.on("data", (chunk) => (body += chunk));
      req.on("end", () => resolve(body ? JSON.parse(body) : {}));
    });
  }

  return listen(async (req, res) => {
    // The real lifeos-api serves cross-origin requests from the Vite dev
    // server's own port - this fake needs the same CORS header, or the
    // browser-side fetch() below fails silently and nothing ever mounts.
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
    if (req.method === "GET" && req.url.startsWith("/api/entity")) {
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(entities));
      return;
    }
    if (req.method === "POST" && req.url === "/api/entity") {
      const row = { id: `ent_${nextId++}`, ...(await readBody(req)) };
      entities.push(row);
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(row));
      return;
    }
    if (req.method === "PATCH" && req.url.startsWith("/api/entity/")) {
      const id = req.url.split("/").pop();
      const patch = await readBody(req);
      entities = entities.map((e) => (e.id === id ? { ...e, ...patch } : e));
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(entities.find((e) => e.id === id)));
      return;
    }
    res.writeHead(404).end();
  });
}

// A fake frontend: polls the given api for module.installed events (mirroring
// useModuleStream.js's real poll fallback) and dispatches the same
// module-mounted:<id> CustomEvent the real moduleRegistry.js emits. `views`
// (issue #121) renders one `data-view-tab` button per declared view; clicking
// one mounts a `data-view-id` node, mirroring ModuleManifestPage.jsx.
function startFakeFrontend(apiUrl, { throwOnLoad = false, views = [] } = {}) {
  const tabsHtml = views.map((v) => `<button data-view-tab="${v.id}" onclick="mountView('${v.id}')">${v.id}</button>`).join("");
  const html = `<!doctype html><html><body>
    ${tabsHtml}
    <div id="view-container"></div>
    <script>
      ${throwOnLoad ? "throw new Error('simulated render crash');" : ""}
      function mountView(id) {
        document.getElementById('view-container').innerHTML = '<div data-view-id="' + id + '"></div>';
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

describe("validateRenderSmoke - happy path", () => {
  it("passes when module-mounted fires with no console/page errors", async () => {
    const { url: apiUrl } = await startFakeApi();
    const { url: frontendUrl } = await startFakeFrontend(apiUrl);

    const result = await validateRenderSmoke(
      "widgets",
      { name: "Widgets" },
      {
        launchApi: async () => ({ url: apiUrl, stop: () => {} }),
        launchFrontend: async () => ({ url: frontendUrl, stop: () => {} }),
      },
    );

    expect(result).toEqual({ valid: true, errors: [] });
  }, 20000);
});

describe("validateRenderSmoke - failures", () => {
  it("fails after the bounded retry when the page throws on load", async () => {
    const { url: apiUrl } = await startFakeApi();
    const { url: frontendUrl } = await startFakeFrontend(apiUrl, { throwOnLoad: true });
    let launchCount = 0;

    const result = await validateRenderSmoke(
      "widgets",
      { name: "Widgets" },
      {
        launchApi: async () => ({ url: apiUrl, stop: () => {} }),
        launchFrontend: async () => {
          launchCount += 1;
          return { url: frontendUrl, stop: () => {} };
        },
      },
    );

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/console\/page errors/);
    expect(launchCount).toBe(2); // one bounded retry - exactly two attempts
  }, 20000);

  it("fails when module-mounted never fires (no arbitrary-timeout success)", async () => {
    // Two full MOUNT_TIMEOUT_MS attempts (the bounded retry) plus overhead
    // genuinely exceeds vitest's default 20s test timeout here.
    const { url: apiUrl } = await startFakeApi();
    // A frontend that never polls/dispatches anything - the event genuinely
    // never fires, distinct from the throw-on-load case above.
    const { url: frontendUrl } = await listen((req, res) => {
      res.writeHead(200, { "content-type": "text/html" }).end("<!doctype html><html><body>idle</body></html>");
    });

    const result = await validateRenderSmoke(
      "widgets",
      { name: "Widgets" },
      {
        launchApi: async () => ({ url: apiUrl, stop: () => {} }),
        launchFrontend: async () => ({ url: frontendUrl, stop: () => {} }),
      },
    );

    expect(result.valid).toBe(false);
    expect(result.errors[0]).toMatch(/did not fire within/);
  }, 30000);

  it("succeeds on the retry when the first attempt fails transiently", async () => {
    const { url: apiUrl } = await startFakeApi();
    const { url: frontendUrl } = await startFakeFrontend(apiUrl);
    let attempt = 0;

    const result = await validateRenderSmoke(
      "widgets",
      { name: "Widgets" },
      {
        launchApi: async () => {
          attempt += 1;
          if (attempt === 1) throw new Error("transient boot failure");
          return { url: apiUrl, stop: () => {} };
        },
        launchFrontend: async () => ({ url: frontendUrl, stop: () => {} }),
      },
    );

    expect(result).toEqual({ valid: true, errors: [] });
    expect(attempt).toBe(2);
  }, 20000);

  it("calls stop() on every launched process even after a failure", async () => {
    const { url: apiUrl } = await startFakeApi();
    const { url: frontendUrl } = await startFakeFrontend(apiUrl, { throwOnLoad: true });
    let apiStops = 0;
    let frontendStops = 0;

    await validateRenderSmoke(
      "widgets",
      { name: "Widgets" },
      {
        launchApi: async () => ({ url: apiUrl, stop: () => (apiStops += 1) }),
        launchFrontend: async () => ({ url: frontendUrl, stop: () => (frontendStops += 1) }),
      },
    );

    expect(apiStops).toBe(2);
    expect(frontendStops).toBe(2);
  }, 20000);
});

describe("validateRenderSmoke - per-view assertions (issue #121)", () => {
  it("passes when every declared view mounts a node", async () => {
    const manifest = {
      id: "widgets",
      name: "Widgets",
      views: [
        { id: "list", label: "List", kind: "list", type: "widget" },
        { id: "board", label: "Board", kind: "board", type: "widget" },
      ],
    };
    const modulePath = await writeModuleFile(manifest);
    const { url: apiUrl } = await startFakeApi();
    const { url: frontendUrl } = await startFakeFrontend(apiUrl, { views: manifest.views });

    const result = await validateRenderSmoke("widgets", { name: "Widgets" }, {
      launchApi: async () => ({ url: apiUrl, stop: () => {} }),
      launchFrontend: async () => ({ url: frontendUrl, stop: () => {} }),
      modulePath,
    });

    expect(result).toEqual({ valid: true, errors: [] });
  }, 30000);

  it("fails cleanly when a declared view never mounts its node", async () => {
    const manifest = {
      id: "widgets",
      name: "Widgets",
      views: [
        { id: "list", label: "List", kind: "list", type: "widget" },
        { id: "broken", label: "Broken", kind: "board", type: "widget" },
      ],
    };
    const modulePath = await writeModuleFile(manifest);
    const { url: apiUrl } = await startFakeApi();
    // The fake frontend only knows how to render a tab/node for "list" -
    // "broken" is declared in the manifest but never gets a live view path,
    // exactly the failure this validator must catch.
    const { url: frontendUrl } = await startFakeFrontend(apiUrl, { views: [manifest.views[0]] });

    const result = await validateRenderSmoke("widgets", { name: "Widgets" }, {
      launchApi: async () => ({ url: apiUrl, stop: () => {} }),
      launchFrontend: async () => ({ url: frontendUrl, stop: () => {} }),
      modulePath,
    });

    expect(result.valid).toBe(false);
  }, 30000);
});
