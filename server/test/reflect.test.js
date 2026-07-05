import { describe, expect, it, vi } from "vitest";
import { distillLesson, looksCorrective } from "../agent/reflect.js";

function makeQueryFn(structured) {
  return vi.fn(async function* () {
    yield { type: "result", structured_output: structured, usage: { input_tokens: 4, output_tokens: 4 } };
  });
}

describe("looksCorrective", () => {
  it.each([
    "always keep drafts under 80 words",
    "never post without approval",
    "don't use em dashes",
    "that was wrong, instead do it this way",
    "next time lead with the TLDR",
    "from now on tag every trade entry",
    "I prefer shorter summaries",
  ])("flags a corrective/preference prompt: %s", (prompt) => {
    expect(looksCorrective(prompt)).toBe(true);
  });

  it.each(["what is the weather", "create a task for tomorrow", "summarize my notes"])(
    "does not flag a plain, non-corrective prompt: %s",
    (prompt) => {
      expect(looksCorrective(prompt)).toBe(false);
    },
  );
});

describe("distillLesson", () => {
  it("emits exactly one well-formed feedback.given event for a corrective, completed turn", async () => {
    const queryFn = makeQueryFn({ rule: "keep drafts under 80 words", confidence: 0.8, kind: "lesson" });
    const httpFn = vi.fn(async () => ({ ok: true, status: 200, data: { id: "evt_1" } }));

    await distillLesson(queryFn, "always keep drafts under 80 words", "completed", "ok, done", {
      httpFn,
      workspaceId: "ws_test",
    });

    expect(httpFn).toHaveBeenCalledTimes(1);
    const [method, path, body] = httpFn.mock.calls[0];
    expect(method).toBe("POST");
    expect(path).toBe("/api/event");
    expect(body).toMatchObject({
      type: "feedback.given",
      actor: "agent",
      workspace_id: "ws_test",
      attrs: {
        feedback: "lesson: keep drafts under 80 words",
        confidence: 0.8,
        source: "agent-reflect",
      },
    });
  });

  it("emits nothing when the model returns a null rule", async () => {
    const queryFn = makeQueryFn({ rule: null, confidence: 0, kind: "lesson" });
    const httpFn = vi.fn(async () => ({ ok: true, status: 200, data: {} }));

    await distillLesson(queryFn, "always be terse", "completed", "done", { httpFn, workspaceId: "ws_test" });

    expect(httpFn).not.toHaveBeenCalled();
  });

  it("skips non-corrective turns entirely - no queryFn call at all", async () => {
    const queryFn = makeQueryFn({ rule: "something", confidence: 0.9, kind: "lesson" });
    const httpFn = vi.fn(async () => ({ ok: true, status: 200, data: {} }));

    await distillLesson(queryFn, "what is the weather today", "completed", "sunny", {
      httpFn,
      workspaceId: "ws_test",
    });

    expect(queryFn).not.toHaveBeenCalled();
    expect(httpFn).not.toHaveBeenCalled();
  });

  it("skips non-completed outcomes even when the prompt is corrective", async () => {
    const queryFn = makeQueryFn({ rule: "something", confidence: 0.9, kind: "lesson" });
    const httpFn = vi.fn(async () => ({ ok: true, status: 200, data: {} }));

    await distillLesson(queryFn, "always tag urgent tasks", "failed", "", { httpFn, workspaceId: "ws_test" });

    expect(queryFn).not.toHaveBeenCalled();
    expect(httpFn).not.toHaveBeenCalled();
  });

  it("never throws and does not affect the caller when the model call fails", async () => {
    const queryFn = vi.fn(async function* () {
      throw new Error("model down");
    });
    const httpFn = vi.fn(async () => ({ ok: true, status: 200, data: {} }));

    await expect(
      distillLesson(queryFn, "always keep it short", "completed", "text", { httpFn, workspaceId: "ws_test" }),
    ).resolves.toBeUndefined();
    expect(httpFn).not.toHaveBeenCalled();
  });

  it("never throws and does not affect the caller when the event write fails", async () => {
    const queryFn = makeQueryFn({ rule: "keep it short", confidence: 0.7, kind: "lesson" });
    const httpFn = vi.fn(async () => {
      throw new Error("api down");
    });

    await expect(
      distillLesson(queryFn, "always keep it short", "completed", "text", { httpFn, workspaceId: "ws_test" }),
    ).resolves.toBeUndefined();
  });
});
