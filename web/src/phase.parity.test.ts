/*
 * Paridade da etapa do plano (spec 094 T03, decisão 3): a mesma fixture do teste Rust
 * `plan_stage_parity_fixture_matches_the_server_rule` (`src/server/attention_tests.rs`), lida por
 * caminho relativo. `planStage === "awaiting"` vale se e somente se o servidor marca
 * `plan_awaiting_approval` no `/api/attention`.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import type { PlanReviewView } from "./api/types";
import { planStage } from "./phase";
import { makeProjectState, makeSpec, makeTask } from "./test-utils";

// `npm test` roda a partir de web/ (o jsdom não expõe import.meta.url como arquivo).
const FIXTURE = resolve(process.cwd(), "../src/server/testdata/plan_stage_parity.json");

interface ParityCase {
  running: boolean;
  turn_open: boolean;
  plan_review: string | null;
  stage: "planning" | "reviewing" | "awaiting" | null;
  plan_awaiting_approval: boolean;
}

interface ParityFixture {
  spec_status: string;
  cases: ParityCase[];
}

const fixture = JSON.parse(readFileSync(FIXTURE, "utf8")) as ParityFixture;

describe("planStage: paridade com o /api/attention (fixture compartilhada)", () => {
  it("a matriz cobre turno × revisão (6) × execução", () => {
    expect(fixture.spec_status).toBe("AWAITING_APPROVAL");
    expect(fixture.cases).toHaveLength(24);
    const reviews = new Set(fixture.cases.map((c) => c.plan_review));
    expect([...reviews].sort()).toEqual([null, "ok", "pending", "running", "stale", "unavailable"].sort());
  });

  it.each(fixture.cases.map((c, i) => [i, c] as const))("linha %i", (_i, c) => {
    const review: PlanReviewView | undefined = c.plan_review === null ? undefined : { status: c.plan_review, issues: [] };
    const spec = {
      ...makeSpec("s1", "Plano", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]),
      ...(review !== undefined ? { plan_review: review } : {}),
    };
    const state = makeProjectState({ running: c.running, specs: [spec] });
    const stage = planStage(state, spec, c.turn_open);
    expect(stage).toBe(c.stage);
    expect(stage === "awaiting").toBe(c.plan_awaiting_approval);
  });
});
