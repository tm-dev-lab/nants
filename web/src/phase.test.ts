import { describe, expect, it } from "vitest";
import { derivePhase, displayPhase, isExecutionMode, isStalledRun, PHASE_LABELS, phaseForProject, pickActiveSpec, planStage, PULSING_PHASES } from "./phase";
import { makeProjectState, makeSpec, makeTask } from "./test-utils";

describe("pickActiveSpec", () => {
  it("prioriza a em execução, depois a aguardando aprovação, depois o rascunho", () => {
    const done = makeSpec("s1", "A", "COMPLETED");
    const draft = makeSpec("s2", "B", "DRAFT");
    const waiting = makeSpec("s3", "C", "AWAITING_APPROVAL");
    const running = makeSpec("s4", "D", "IN_PROGRESS");
    expect(pickActiveSpec([done, draft, waiting, running])?.id).toBe("s4");
    expect(pickActiveSpec([done, draft, waiting])?.id).toBe("s3");
    expect(pickActiveSpec([done, draft])?.id).toBe("s2");
    expect(pickActiveSpec([done])?.id).toBe("s1");
    expect(pickActiveSpec([])).toBeNull();
  });
});

describe("derivePhase", () => {
  it("deriva a fase do estado quando não há evento", () => {
    expect(derivePhase(null)).toBe("idle");
    expect(derivePhase(makeProjectState({ running: true }))).toBe("executing");
    expect(derivePhase(makeProjectState({ specs: [makeSpec("s1", "P", "DRAFT")] }))).toBe("planning");
    expect(
      derivePhase(
        makeProjectState({ specs: [makeSpec("s1", "P", "IN_PROGRESS", [makeTask("T01", "t", "BLOCKED")])] }),
      ),
    ).toBe("blocked");
  });
});

describe("displayPhase e isExecutionMode", () => {
  it("plano aguardando aprovação mostra aguardando; execução depende da spec ou do runner", () => {
    const waiting = makeSpec("s1", "P", "AWAITING_APPROVAL");
    expect(displayPhase("planning", waiting)).toBe("waiting_approval");
    expect(displayPhase("idle", waiting)).toBe("waiting_approval");
    expect(displayPhase("executing", waiting)).toBe("executing");
    expect(isExecutionMode(makeProjectState(), waiting)).toBe(false);
    expect(isExecutionMode(makeProjectState(), makeSpec("s1", "P", "IN_PROGRESS"))).toBe(true);
    expect(isExecutionMode(makeProjectState({ running: true }), null)).toBe(true);
  });

  it("spec IN_PROGRESS sem runner ativo nunca aparece como ociosa", () => {
    const running = makeSpec("s1", "P", "IN_PROGRESS");
    const state = makeProjectState({ running: false, specs: [running] });
    expect(derivePhase(state)).toBe("executing");
    expect(displayPhase("idle", running)).toBe("executing");
    expect(displayPhase("planning", running)).toBe("executing");
    expect(isExecutionMode(state, running)).toBe(true);
  });
});

describe("derivePhase: spec integrada", () => {
  it("spec COMPLETED ativa (sem outra mais prioritária) é integrada", () => {
    expect(derivePhase(makeProjectState({ specs: [makeSpec("s1", "P", "COMPLETED")] }))).toBe("integrated");
  });

  it("COMPLETED com outra spec em rascunho: a ativa é a DRAFT, então planejando", () => {
    const state = makeProjectState({
      specs: [makeSpec("s1", "A", "COMPLETED"), makeSpec("s2", "B", "DRAFT")],
    });
    expect(derivePhase(state)).toBe("planning");
  });

  it("COMPLETED com tarefa BLOCKED na ativa: bloqueado", () => {
    const state = makeProjectState({
      specs: [makeSpec("s1", "P", "COMPLETED", [makeTask("T01", "t", "BLOCKED")])],
    });
    expect(derivePhase(state)).toBe("blocked");
  });

  it("execução em andamento tem precedência sobre integrada", () => {
    const state = makeProjectState({ running: true, specs: [makeSpec("s1", "P", "COMPLETED")] });
    expect(derivePhase(state)).toBe("executing");
  });

  it("displayPhase mantém 'integrated' para spec COMPLETED", () => {
    expect(displayPhase("integrated", makeSpec("s1", "P", "COMPLETED"))).toBe("integrated");
    expect(displayPhase("idle", makeSpec("s1", "P", "COMPLETED"))).toBe("idle");
  });
});

describe("isStalledRun / fase parada", () => {
  const inProg = (tasks: ReturnType<typeof makeTask>[]) =>
    makeProjectState({ specs: [makeSpec("s1", "P", "IN_PROGRESS", tasks)] });
  it("INTEGRATING sem run é parada; BLOCKED, running e tudo DONE não", () => {
    const st = inProg([makeTask("T01", "t", "INTEGRATING")]);
    expect(isStalledRun(st, pickActiveSpec(st.specs))).toBe(true);
    const bl = inProg([makeTask("T01", "t", "BLOCKED")]);
    expect(isStalledRun(bl, pickActiveSpec(bl.specs))).toBe(false);
    const run = makeProjectState({ running: true, specs: st.specs });
    expect(isStalledRun(run, pickActiveSpec(run.specs))).toBe(false);
    const done = inProg([makeTask("T01", "t", "DONE")]);
    expect(isStalledRun(done, pickActiveSpec(done.specs))).toBe(false);
  });
  it("pílula mostra Parado, inclusive com fase armazenada executing", () => {
    const st = inProg([makeTask("T01", "t", "INTEGRATING")]);
    expect(phaseForProject("executing", st)).toBe("stalled");
    expect(PHASE_LABELS.stalled).toBe("Parado");
    expect(phaseForProject("executing", makeProjectState({ running: true, specs: st.specs }))).toBe("executing");
  });
});

describe("turno do Arquiteto ativo (spec 081/T03)", () => {
  const waiting = makeSpec("s1", "P", "AWAITING_APPROVAL");
  it("com turno ativo o plano gravado como AWAITING_APPROVAL ainda é Planejando", () => {
    expect(displayPhase("planning", waiting, true)).toBe("planning");
    expect(displayPhase("idle", waiting, true)).toBe("planning");
    const state = makeProjectState({ specs: [waiting] });
    expect(phaseForProject(undefined, state, true)).toBe("planning");
    expect(phaseForProject("planning", state, true)).toBe("planning");
  });
  it("quando o turno termina volta a Aguardando sua aprovação", () => {
    const state = makeProjectState({ specs: [waiting] });
    expect(phaseForProject(undefined, state, false)).toBe("waiting_approval");
    expect(phaseForProject(undefined, state)).toBe("waiting_approval");
  });
  it("pedido de permissão pendente (fase armazenada) continua visível durante o turno", () => {
    const state = makeProjectState({ specs: [makeSpec("s1", "P", "DRAFT")] });
    expect(phaseForProject("waiting_approval", state, true)).toBe("waiting_approval");
  });
  it("turno durante a execução não muda a fase", () => {
    const state = makeProjectState({ running: true, specs: [makeSpec("s1", "P", "IN_PROGRESS")] });
    expect(phaseForProject("executing", state, true)).toBe("executing");
  });
});

describe("pílula de fase", () => {
  it("só pulsa em planning, reviewing_plan, executing, finalizing e waiting_approval", () => {
    expect([...PULSING_PHASES].sort()).toEqual(["executing", "finalizing", "planning", "reviewing_plan", "waiting_approval"]);
  });
});

describe("phaseForProject: fase exibida única (pílula e seletor de espaços)", () => {
  const withState = (specs: ReturnType<typeof makeSpec>[]) => makeProjectState({ specs });

  it("sem estado carregado e sem fase armazenada: ociosa; com fase armazenada, ela", () => {
    expect(phaseForProject(undefined, null)).toBe("idle");
    expect(phaseForProject("executing", null)).toBe("executing");
  });

  it("sem fase armazenada, deriva do estado (COMPLETED vira integrada); a armazenada prevalece", () => {
    expect(phaseForProject(undefined, withState([makeSpec("s1", "A", "COMPLETED")]))).toBe("integrated");
    expect(phaseForProject("executing", withState([makeSpec("s1", "A", "COMPLETED")]))).toBe("executing");
  });

  it("spec COMPLETED com relatório pendente: Finalizando relatório, mesmo com fase armazenada", () => {
    const state = makeProjectState({ specs: [makeSpec("s1", "A", "COMPLETED")], report_pending: true });
    expect(phaseForProject(undefined, state)).toBe("finalizing");
    expect(phaseForProject("integrated", state)).toBe("finalizing");
    expect(PHASE_LABELS.finalizing).toBe("Finalizando relatório");
    const done = makeProjectState({ specs: [makeSpec("s1", "A", "COMPLETED")], report_pending: false });
    expect(phaseForProject("integrated", done)).toBe("integrated");
  });

  it("aguardando aprovação continua prevalecendo sobre ociosa/planejando", () => {
    expect(phaseForProject(undefined, withState([makeSpec("s1", "A", "AWAITING_APPROVAL")]))).toBe("waiting_approval");
  });
});

describe("planStage e a fase exibida (spec 094, decisão 3)", () => {
  const withReview = (status: string | null) => ({
    ...makeSpec("s1", "P", "AWAITING_APPROVAL", [makeTask("T01", "t", "TODO")]),
    ...(status === null ? {} : { plan_review: { status, issues: [] } }),
  });

  it("turno aberto = planning, mesmo com o rascunho gravado e a revisão pendente", () => {
    const spec = withReview("pending");
    const state = makeProjectState({ specs: [spec] });
    expect(planStage(state, spec, true)).toBe("planning");
    expect(phaseForProject(undefined, state, true)).toBe("planning");
    expect(PHASE_LABELS.planning).toBe("Planejando");
  });

  it("sem turno e revisão pending/running = reviewing (pílula 'Revisando o plano')", () => {
    for (const status of ["pending", "running"]) {
      const spec = withReview(status);
      const state = makeProjectState({ specs: [spec] });
      expect(planStage(state, spec, false)).toBe("reviewing");
      expect(phaseForProject("planning", state, false)).toBe("reviewing_plan");
    }
    expect(PHASE_LABELS.reviewing_plan).toBe("Revisando o plano");
  });

  it("sem turno e revisão concluída (ou ausente) = awaiting", () => {
    for (const status of ["ok", "stale", "unavailable", null]) {
      const spec = withReview(status);
      const state = makeProjectState({ specs: [spec] });
      expect(planStage(state, spec, false)).toBe("awaiting");
      expect(phaseForProject(undefined, state, false)).toBe("waiting_approval");
    }
  });

  it("com execução, sem estado, sem spec ou com spec fora do plano: null", () => {
    const spec = withReview(null);
    expect(planStage(makeProjectState({ running: true, specs: [spec] }), spec, false)).toBeNull();
    expect(planStage(null, spec, false)).toBeNull();
    expect(planStage(makeProjectState(), null, true)).toBeNull();
    const running = makeSpec("s1", "P", "IN_PROGRESS");
    expect(planStage(makeProjectState({ specs: [running] }), running, true)).toBeNull();
  });

  it("rascunho (DRAFT): planning só com o turno aberto", () => {
    const draft = makeSpec("s1", "P", "DRAFT");
    const state = makeProjectState({ specs: [draft] });
    expect(planStage(state, draft, true)).toBe("planning");
    expect(planStage(state, draft, false)).toBeNull();
  });

  it("pedido de permissão pendente (fase armazenada) continua visível em qualquer etapa", () => {
    const spec = withReview("running");
    expect(phaseForProject("waiting_approval", makeProjectState({ specs: [spec] }), false)).toBe("waiting_approval");
  });
});
