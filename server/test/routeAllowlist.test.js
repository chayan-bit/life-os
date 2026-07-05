import { describe, expect, it } from "vitest";
import { isRouteAllowed } from "../lib/routeAllowlist.js";

describe("isRouteAllowed - the T2 generated-tool route whitelist", () => {
  it("allows GET/POST /api/entity and /api/entity/:id", () => {
    expect(isRouteAllowed("GET", "/api/entity")).toBe(true);
    expect(isRouteAllowed("GET", "/api/entity?module=trading")).toBe(true);
    expect(isRouteAllowed("GET", "/api/entity/ent_1")).toBe(true);
    expect(isRouteAllowed("POST", "/api/entity")).toBe(true);
    expect(isRouteAllowed("POST", "/api/entity/ent_1")).toBe(true);
  });

  it("allows GET/POST /api/edge", () => {
    expect(isRouteAllowed("GET", "/api/edge?src_id=ent_1")).toBe(true);
    expect(isRouteAllowed("POST", "/api/edge")).toBe(true);
  });

  it("allows GET /api/search but not POST", () => {
    expect(isRouteAllowed("GET", "/api/search?q=hello")).toBe(true);
    expect(isRouteAllowed("POST", "/api/search")).toBe(false);
  });

  it("allows POST /api/memory/recall", () => {
    expect(isRouteAllowed("POST", "/api/memory/recall")).toBe(true);
    expect(isRouteAllowed("GET", "/api/memory/recall")).toBe(false);
  });

  it("allows POST /api/event", () => {
    expect(isRouteAllowed("POST", "/api/event")).toBe(true);
  });

  it("allows POST /api/browser/scrape", () => {
    expect(isRouteAllowed("POST", "/api/browser/scrape")).toBe(true);
  });

  it("denies configs, module-request, jobs, llm, agent, whatsapp, storage, travel, notion", () => {
    const denied = [
      ["GET", "/api/configs"],
      ["POST", "/api/module-request"],
      ["GET", "/api/jobs"],
      ["POST", "/api/llm"],
      ["POST", "/api/agent"],
      ["POST", "/api/whatsapp/send"],
      ["POST", "/api/storage/upload"],
      ["GET", "/api/travel/flights"],
      ["GET", "/api/notion/pages"],
    ];
    for (const [method, path] of denied) {
      expect(isRouteAllowed(method, path)).toBe(false);
    }
  });

  it("denies anything with 'order' or 'broker'", () => {
    const denied = [
      ["POST", "/api/orders/place"],
      ["POST", "/api/orders"],
      ["GET", "/api/broker/status"],
      ["POST", "/api/entity/order_1/order"],
    ];
    for (const [method, path] of denied) {
      expect(isRouteAllowed(method, path)).toBe(false);
    }
  });

  it("denies connections/secrets routes", () => {
    expect(isRouteAllowed("POST", "/api/connections/revoke")).toBe(false);
    expect(isRouteAllowed("GET", "/api/secrets/foo")).toBe(false);
  });

  it("fails closed on non-string/empty input", () => {
    expect(isRouteAllowed(undefined, "/api/entity")).toBe(false);
    expect(isRouteAllowed("GET", undefined)).toBe(false);
    expect(isRouteAllowed("GET", "")).toBe(false);
    expect(isRouteAllowed(null, null)).toBe(false);
  });

  it("is case-insensitive on method", () => {
    expect(isRouteAllowed("get", "/api/entity")).toBe(true);
    expect(isRouteAllowed("post", "/api/event")).toBe(true);
  });
});
