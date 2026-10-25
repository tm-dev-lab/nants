/*
 * Execução pausada ou parada na janela Conversa (spec 093, T06), montada pelo shell, com o
 * bloqueio por prontidão
 * (`readiness.execute`) e o 412 `prereq_missing`.
 */
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { parseApiEvent } from "../../api/events";
import type { ProjectState, RecoveryOption, SpecView } from "../../api/types";
import {
  APPROVE_REASON_MESSAGES,
  RECOVERY_CONFIRM,
  RECOVERY_ENV_WARNING,
  RECOVERY_GUIDANCE_LABEL,
  RECOVERY_GUIDANCE_MAX_BYTES,
  RECOVERY_GUIDANCE_SEND,
  RECOVERY_OTHER_PATH,
  RECOVERY_REASON_MESSAGES,
} from "../../copy";
import { appStore } from "../../store";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { notReady, READY, renderShell, settle } from "../../desktop/shell-test-utils";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const PID = "p1";
const RESUME = /\/api\/projects\/p1\/specs\/s1\/resume$/;
const CHOICE = /\/api\/projects\/p1\/specs\/s1\/tasks\/T01\/recovery-choice$/;
const REASON = "alterações não commitadas em .sdd: .sdd/settings.yaml";

function pausedSpec(extra: Partial<SpecView["tasks"][number]> = {}): SpecView {
  return makeSpec("s1", "Plano", "IN_PROGRESS", [makeTask("T01", "Middleware", "BLOCKED", { attempts: 3, next_action: REASON, ...extra })]);
}

function stateWith(spec: SpecView, extra: Partial<ProjectState> = {}): ProjectState {
  return makeProjectState({ id: PID, name: "Loja", specs: [spec], ...extra });
}

async function open(state: ProjectState, readiness = READY) {
  const shell = await renderShell({ url: "/?space=p1", projects: [makeProject(PID, "Loja")], states: { p1: state }, readiness });
  await settle();
  return shell;
}

function card(): HTMLElement {
  const el = document.querySelector<HTMLElement>("[data-paused-card]");
  if (el === null) throw new Error("cartão ausente");
  return el;
}

describe("Execução pausada na Conversa", () => {
  it("aparece com a tarefa BLOCKED e o motivo, na lista de mensagens, com Retomar e a orientação livre", async () => {
    await open(stateWith(pausedSpec()));
    const article = card();
    expect(within(article).getByText("Execução pausada")).toBeTruthy();
    // O id em mono (C-08) e o resto em sem serifa: o texto da linha continua "T01 bloqueada".
    expect(within(article).getByText((_, el) => el?.classList.contains("nd-error-inline") === true && el.textContent === "T01 bloqueada")).toBeTruthy();
    expect(within(article).getByText(REASON)).toBeTruthy();
    expect(within(article).getByText("Arquiteto")).toBeTruthy();
    expect(screen.getByRole("list", { name: "Mensagens da conversa" }).contains(article)).toBe(true);
    expect(within(article).getByRole("textbox", { name: "Outra orientação" })).toBeTruthy();
    expect(findForbiddenGlyphs(article)).toEqual([]);
  });

  it("parada sem bloqueio mostra Continuar", async () => {
    await open(stateWith(makeSpec("s1", "Plano", "IN_PROGRESS", [makeTask("T01", "A", "IN_PROGRESS")])));
    expect(within(card()).getByText("Execução parada")).toBeTruthy();
    expect(within(card()).getByRole("button", { name: "Continuar" })).toBeTruthy();
  });

  it("Retomar chama resume uma vez, mesmo com duplo clique, e relê o estado", async () => {
    const { server } = await open(stateWith(pausedSpec()));
    server.reply("POST", RESUME, () => ({ status: 202, body: { ok: true, spec_id: "s1" } }));
    const statesBefore = server.count("GET", /\/api\/projects\/p1\/state$/);
    const button = within(card()).getByRole("button", { name: "Retomar" });
    fireEvent.click(button);
    fireEvent.click(button);
    await settle();
    expect(server.count("POST", RESUME)).toBe(1);
    expect(server.count("GET", /\/api\/projects\/p1\/state$/)).toBeGreaterThan(statesBefore);
  });

  it("409 com motivo conhecido mostra a mensagem fixa; o cartão continua", async () => {
    const { server } = await open(stateWith(pausedSpec()));
    server.reply("POST", RESUME, () => ({ status: 409, body: { error: "a base tem alterações", reason: "base_dirty" } }));
    fireEvent.click(within(card()).getByRole("button", { name: "Retomar" }));
    await settle();
    expect(within(card()).getByRole("alert").textContent).toContain(APPROVE_REASON_MESSAGES.base_dirty ?? "");
    expect(within(card()).getByText("Execução pausada")).toBeTruthy();
  });

  it("422 de agente não configurado oferece Configurar na aba Agentes", async () => {
    const { server } = await open(stateWith(pausedSpec()));
    server.reply("POST", RESUME, () => ({ status: 422, body: { error: "agente developer não configurado", code: "developer_not_configured" } }));
    fireEvent.click(within(card()).getByRole("button", { name: "Retomar" }));
    await settle();
    const alert = within(card()).getByRole("alert");
    expect(alert.textContent).toContain("Os agentes ainda não estão configurados.");
    expect(within(alert).getByRole("button", { name: "Configurar: Agentes" }).getAttribute("data-settings-tab")).toBe("agents");
  });

  it("412 prereq_missing: o que falta com Configurar, e a prontidão é relida", async () => {
    const { server } = await open(stateWith(pausedSpec()));
    server.reply("POST", RESUME, () => ({ status: 412, body: { error: "prereq_missing", missing: ["docker_daemon"] } }));
    const before = server.count("GET", /^\/api\/readiness$/);
    fireEvent.click(within(card()).getByRole("button", { name: "Retomar" }));
    await settle();
    const alert = within(card()).getByRole("alert");
    expect(alert.textContent).toContain("Docker daemon");
    const configure = within(alert).getByRole("button", { name: "Configurar: Docker daemon" });
    expect(configure.getAttribute("data-settings-tab")).toBe("machine");
    expect(server.count("GET", /^\/api\/readiness$/)).toBeGreaterThan(before);
    fireEvent.click(configure);
    await settle();
    expect(screen.getByRole("region", { name: "Configurações" })).toBeTruthy();
  });

  it("sem readiness.execute: Retomar e as escolhas ficam desabilitados com a lista do que falta", async () => {
    const options = [{ id: "o1", kind: "guidance", label: "Usar outra porta", text: "use 8081" }];
    await open(stateWith(pausedSpec({ recovery_options: options, recovery_options_analysis: 7 } as never)), notReady());
    const article = card();
    expect((within(article).getByRole("button", { name: "Retomar" }) as HTMLButtonElement).disabled).toBe(true);
    expect((within(article).getByRole("button", { name: "Escolher: Usar outra porta" }) as HTMLButtonElement).disabled).toBe(true);
    const notice = article.querySelector<HTMLElement>("[data-readiness-notice]");
    expect(notice?.textContent).toContain("Docker daemon");
  });

  it("opção de escalada pede confirmação e envia a escolha com a análise", async () => {
    const options = [{ id: "o1", kind: "guidance", label: "Usar outra porta", text: "use 8081" }];
    const { server } = await open(stateWith(pausedSpec({ recovery_options: options, recovery_options_analysis: 7 } as never)));
    server.reply("POST", CHOICE, () => ({ status: 202, body: { ok: true } }));
    fireEvent.click(within(card()).getByRole("button", { name: "Escolher: Usar outra porta" }));
    expect(server.count("POST", CHOICE)).toBe(0);
    fireEvent.click(within(card()).getByRole("button", { name: "Aplicar e retomar" }));
    await waitFor(() => expect(server.count("POST", CHOICE)).toBe(1));
    expect(server.calls.find((c) => c.method === "POST" && CHOICE.test(c.url))?.body).toEqual({ option_id: "o1", analysis: 7 });
  });

  it("motivo com HTML aparece como texto", async () => {
    const hostile = "<img src=x onerror=alert(1)> <script>window.__pwn = 1</script>";
    await open(stateWith(makeSpec("s1", "Plano", "IN_PROGRESS", [makeTask("T01", "A", "BLOCKED", { next_action: hostile })])));
    expect(within(card()).getByText(hostile)).toBeTruthy();
    expect(document.querySelector("img[src='x']")).toBeNull();
    expect(document.querySelector("script")).toBeNull();
  });

  it("não aparece com execução ativa", async () => {
    await open(stateWith(pausedSpec(), { running: true, run: { spec_id: "s1", status: "running" } as never }));
    expect(document.querySelector("[data-paused-card]")).toBeNull();
  });
});

const OPTIONS: RecoveryOption[] = [
  { id: "o1", label: "Usar npx tsc", kind: "config", test_command: "npx tsc", diff: "Antes:\ntsc\n\nDepois:\nnpx tsc <b>x</b>" },
  { id: "o2", label: "Reduzir escopo", kind: "revise_task", objective: "Só a API", acceptance: ["GET responde"] },
];

function withOptions(options: RecoveryOption[] = OPTIONS, analysis = 7): SpecView {
  return pausedSpec({ recovery_options: options, recovery_options_analysis: analysis });
}

function applyEvent(type: string, payload: Record<string, unknown>): void {
  const event = parseApiEvent({ event_type: type, project_id: PID, payload });
  if (event === null) throw new Error("evento inválido no teste");
  act(() => appStore.applyEvent(event));
}

async function chooseOption(label: string): Promise<void> {
  fireEvent.click(within(card()).getByRole("button", { name: `Escolher: ${label}` }));
  await act(async () => {
    fireEvent.click(within(card()).getByRole("button", { name: RECOVERY_CONFIRM }));
  });
  await settle();
}

const choiceBodies = (server: { calls: { method: string; url: string; body: unknown }[] }, pattern = CHOICE) =>
  server.calls.filter((c) => c.method === "POST" && pattern.test(c.url)).map((c) => c.body);

describe("Execução pausada: opções de recuperação", () => {
  it("409 de orçamento mostra o motivo em role=alert; nada é aplicado", async () => {
    const { server } = await open(stateWith(withOptions()));
    server.reply("POST", CHOICE, () => ({ status: 409, body: { error: "x", reason: "budget_exceeded" } }));
    await chooseOption("Usar npx tsc");
    expect(within(card()).getByRole("alert").textContent).toContain(RECOVERY_REASON_MESSAGES.budget_exceeded ?? "");
  });

  it("409 sem reason (opção vencida) mostra a mensagem curta", async () => {
    const { server } = await open(stateWith(withOptions()));
    server.reply("POST", CHOICE, () => ({ status: 409, body: { error: "stale" } }));
    await chooseOption("Reduzir escopo");
    expect(within(card()).getByRole("alert").textContent).toMatch(/não valem mais/);
  });

  it("orientação livre (atrás de 'Prefiro outro caminho') respeita o limite de bytes e envia guidance", async () => {
    const { server } = await open(stateWith(withOptions()));
    server.reply("POST", CHOICE, () => ({ status: 202, body: { ok: true } }));
    expect(within(card()).queryByRole("textbox", { name: RECOVERY_GUIDANCE_LABEL })).toBeNull();
    fireEvent.click(within(card()).getByRole("button", { name: RECOVERY_OTHER_PATH }));
    const box = within(card()).getByRole("textbox", { name: RECOVERY_GUIDANCE_LABEL });
    const send = within(card()).getByRole("button", { name: RECOVERY_GUIDANCE_SEND }) as HTMLButtonElement;
    expect(send.disabled).toBe(true);
    fireEvent.change(box, { target: { value: "é".repeat(1100) } });
    expect(within(card()).getByText(`2200/${RECOVERY_GUIDANCE_MAX_BYTES}`)).toBeTruthy();
    expect(send.disabled).toBe(true);
    fireEvent.change(box, { target: { value: "use o binário local" } });
    await act(async () => {
      fireEvent.click(send);
    });
    await settle();
    expect(choiceBodies(server)).toEqual([{ guidance: "use o binário local" }]);
  });

  it("opção environment: arquivos propostos recolhíveis em texto e a confirmação avisa do ambiente", async () => {
    const env: RecoveryOption = {
      id: "e1",
      label: "Instalar curl na imagem",
      kind: "environment",
      files: [{ path: ".nants/Dockerfile", content: "FROM x\nRUN curl <i>y</i>" }],
      diff: "--- .nants/Dockerfile\n+RUN apk add curl <b>z</b>",
    };
    const { server } = await open(stateWith(withOptions([env], 3)));
    server.reply("POST", CHOICE, () => ({ status: 202, body: { ok: true } }));
    const toggle = within(card()).getByRole("button", { name: /Ver arquivos propostos/ });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(card().querySelector("pre")).toBeNull();
    fireEvent.click(toggle);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    const pre = card().querySelector("pre");
    expect(pre?.textContent).toContain("+RUN apk add curl <b>z</b>");
    expect(pre?.querySelector("b")).toBeNull();
    fireEvent.click(within(card()).getByRole("button", { name: "Escolher: Instalar curl na imagem" }));
    expect(within(card()).getByText(RECOVERY_ENV_WARNING)).toBeTruthy();
    await act(async () => {
      fireEvent.click(within(card()).getByRole("button", { name: RECOVERY_CONFIRM }));
    });
    await settle();
    expect(choiceBodies(server)).toEqual([{ option_id: "e1", analysis: 3 }]);
  });

  it("depois de recarregar: análise reidratada do diálogo, opções do estado e orientação funcionam", async () => {
    const analysis = ["Análise de recuperação #1 (ESCALATE)", "- Causa raiz: porta ocupada", "- Contexto:", "  - comando: cargo test"].join("\n");
    const dialog = [{ seq: 1, ts: 1_760_000_000, agent: "architect", task_id: "T01", attempt: null, text: analysis }];
    const shell = await renderShell({
      url: "/?space=p1",
      projects: [makeProject(PID, "Loja")],
      states: { p1: stateWith(withOptions()) },
      readiness: READY,
      dialog,
    });
    await settle();
    shell.server.reply("POST", CHOICE, () => ({ status: 202, body: { ok: true } }));
    expect(within(card()).getByText("O que aconteceu")).toBeTruthy();
    expect(within(card()).getAllByText("cargo test").length).toBeGreaterThan(0);
    await chooseOption("Usar npx tsc");
    fireEvent.click(within(card()).getByRole("button", { name: RECOVERY_OTHER_PATH }));
    fireEvent.change(within(card()).getByRole("textbox", { name: RECOVERY_GUIDANCE_LABEL }), { target: { value: "tente X" } });
    await act(async () => {
      fireEvent.click(within(card()).getByRole("button", { name: RECOVERY_GUIDANCE_SEND }));
    });
    await settle();
    expect(choiceBodies(shell.server)).toEqual([{ option_id: "o1", analysis: 7 }, { guidance: "tente X" }]);
  });

  it("duas tarefas BLOCKED, opções só na T02: o cartão indica o alvo e o POST vai para a T02", async () => {
    const spec = makeSpec("s1", "Plano", "IN_PROGRESS", [
      makeTask("T01", "A", "BLOCKED", { next_action: "m1" }),
      makeTask("T02", "B", "BLOCKED", { next_action: "m2", recovery_options: OPTIONS, recovery_options_analysis: 4 }),
    ]);
    const { server } = await open(stateWith(spec));
    const t02 = /\/api\/projects\/p1\/specs\/s1\/tasks\/T02\/recovery-choice$/;
    server.reply("POST", t02, () => ({ status: 202, body: { ok: true } }));
    expect(within(card()).getByText("Para")).toBeTruthy();
    await chooseOption("Usar npx tsc");
    expect(choiceBodies(server, t02)).toEqual([{ option_id: "o1", analysis: 4 }]);
    expect(server.count("POST", CHOICE)).toBe(0);
  });

  it("confirmação aberta é descartada quando a análise muda; a escolha seguinte usa a análise nova", async () => {
    const { server } = await open(stateWith(withOptions()));
    server.reply("POST", CHOICE, () => ({ status: 202, body: { ok: true } }));
    fireEvent.click(within(card()).getByRole("button", { name: "Escolher: Usar npx tsc" }));
    expect(within(card()).getByRole("button", { name: RECOVERY_CONFIRM })).toBeTruthy();
    applyEvent("recovery_decided", { spec_id: "s1", task_id: "T01", cycle: 2, decision: "ESCALATE", options: OPTIONS, options_analysis: 8 });
    await waitFor(() => expect(within(card()).queryByRole("button", { name: RECOVERY_CONFIRM })).toBeNull());
    expect(server.count("POST", CHOICE)).toBe(0);
    await chooseOption("Usar npx tsc");
    expect(choiceBodies(server)).toEqual([{ option_id: "o1", analysis: 8 }]);
  });
});

describe("Execução pausada: quando aparece", () => {
  it("lista cada tarefa BLOCKED e omite o motivo quando ausente", async () => {
    await open(stateWith(makeSpec("s1", "Plano", "IN_PROGRESS", [makeTask("T01", "A", "BLOCKED", { next_action: "motivo um" }), makeTask("T02", "B", "BLOCKED")])));
    const article = card();
    // O id em mono (C-08) e o resto em sem serifa: o texto da linha continua "T01 bloqueada".
    expect(within(article).getByText((_, el) => el?.classList.contains("nd-error-inline") === true && el.textContent === "T01 bloqueada")).toBeTruthy();
    expect(within(article).getByText((_, el) => el?.classList.contains("nd-error-inline") === true && el.textContent === "T02 bloqueada")).toBeTruthy();
    expect(within(article).getAllByText("motivo um")).toHaveLength(1);
  });

  it.each([
    ["há run ativo", stateWith(pausedSpec(), { running: true, run: { spec_id: "s1", status: "running" } as never })],
    ["runner ocupado sem flag running", stateWith(pausedSpec(), { run: { spec_id: "s1", status: "running" } as never })],
    ["spec aguardando aprovação", stateWith(makeSpec("s1", "Plano", "AWAITING_APPROVAL", [makeTask("T01", "A", "BLOCKED")]))],
    ["spec em rascunho", stateWith(makeSpec("s1", "Plano", "DRAFT", [makeTask("T01", "A", "BLOCKED")]))],
    ["spec concluída", stateWith(makeSpec("s1", "Plano", "COMPLETED", [makeTask("T01", "A", "BLOCKED")]))],
  ])("não aparece quando %s", async (_label, state) => {
    await open(state);
    expect(document.querySelector("[data-paused-card]")).toBeNull();
    expect(screen.queryByRole("button", { name: "Retomar" })).toBeNull();
  });

  it("sucesso (202) relê o estado e o cartão some quando há run ativo", async () => {
    const { server } = await open(stateWith(pausedSpec()));
    server.reply("POST", RESUME, () => {
      server.model.states.p1 = stateWith(pausedSpec(), { running: true, run: { spec_id: "s1", status: "running" } as never });
      return { status: 202, body: { ok: true, spec_id: "s1" } };
    });
    fireEvent.click(within(card()).getByRole("button", { name: "Retomar" }));
    await settle();
    expect(document.querySelector("[data-paused-card]")).toBeNull();
  });
});

describe("Execução pausada: falhas da retomada", () => {
  it("409 de cada tipo mostra a mensagem fixa e o cartão continua", async () => {
    const cases: Array<[string, { error: string; code?: string; reason?: string }, string]> = [
      ["spec não está em andamento", { error: "spec não está em andamento", code: "spec_not_in_progress" }, "spec não está em andamento"],
      ["não há tarefa bloqueada", { error: "não há tarefa bloqueada para retomar", code: "no_blocked_task" }, "não há tarefa bloqueada para retomar"],
      ["execução em andamento", { error: "já existe uma execução em andamento neste projeto", code: "run_active" }, "já existe uma execução em andamento neste projeto"],
      ["spec alterada", { error: "spec foi alterada durante a aprovação; tente novamente", code: "spec_changed_approval" }, "spec foi alterada durante a aprovação; tente novamente"],
      ["tarefas alteradas", { error: "tarefas foram alteradas durante a retomada; tente novamente", code: "tasks_changed_resume" }, "tarefas foram alteradas durante a retomada; tente novamente"],
      ["base suja", { error: "a base tem alterações não commitadas", reason: "base_dirty" }, APPROVE_REASON_MESSAGES.base_dirty ?? ""],
      ["sem ambiente docker", { error: "x", reason: "environment_not_docker" }, APPROVE_REASON_MESSAGES.environment_not_docker ?? ""],
      ["Docker indisponível", { error: "x", reason: "docker_unavailable" }, APPROVE_REASON_MESSAGES.docker_unavailable ?? ""],
    ];
    for (const [label, body, expected] of cases) {
      cleanup();
      const { server } = await open(stateWith(pausedSpec()));
      server.reply("POST", RESUME, () => ({ status: 409, body }));
      fireEvent.click(within(card()).getByRole("button", { name: "Retomar" }));
      await settle();
      expect({ label, text: within(card()).getByRole("alert").textContent }).toEqual({ label, text: expect.stringContaining(expected) as unknown as string });
      expect(within(card()).getByText("Execução pausada")).toBeTruthy();
    }
  });

  it("5xx e falha de rede mostram mensagem fixa (nunca o corpo) e liberam o botão", async () => {
    const { server, fetchMock } = await open(stateWith(pausedSpec()));
    server.reply("POST", RESUME, () => ({ status: 500, body: { error: "boom interno" } }));
    fireEvent.click(within(card()).getByRole("button", { name: "Retomar" }));
    await settle();
    expect(within(card()).getByRole("alert").textContent).toContain("falha interna do servidor");
    expect(card().textContent).not.toContain("boom interno");
    expect((within(card()).getByRole("button", { name: "Retomar" }) as HTMLButtonElement).disabled).toBe(false);

    const base = fetchMock.getMockImplementation();
    fetchMock.mockImplementation(async (input: string | URL | Request, init?: RequestInit) => {
      if (RESUME.test(String(input))) throw new TypeError("network down");
      if (base === undefined) throw new Error("fetch ausente");
      return base(input, init);
    });
    fireEvent.click(within(card()).getByRole("button", { name: "Retomar" }));
    await settle();
    expect(within(card()).getByRole("alert").textContent).toContain("falha de comunicação com o servidor");
  });
});

describe("Execução pausada: natureza do bloqueio e recuperação", () => {
  it("quality + ESCALATE mostra a análise da spec e Retomar manualmente", async () => {
    await open(stateWith(pausedSpec()));
    applyEvent("agent_message", { agent: "architect", text: "Causa raiz: <b>X</b>. Proposta: Y", task_id: "T01", spec_id: "s1", seq: 1 });
    applyEvent("agent_message", { agent: "architect", text: "Causa raiz: Z. Proposta: W", task_id: "T01", spec_id: "s1", seq: 2 });
    applyEvent("agent_message", { agent: "architect", text: "outra spec", task_id: "T01", spec_id: "s2", seq: 3 });
    applyEvent("recovery_decided", { spec_id: "s1", task_id: "T01", cycle: 1, decision: "ESCALATE" });
    applyEvent("project_status", { status: "blocked", task_id: "T01", reason: "r", kind: "quality" });
    const article = card();
    expect(within(article).getByText("O Arquiteto analisou e escalou para você")).toBeTruthy();
    expect(within(article).getAllByText(/Causa raiz: Z\. Proposta: W/).length).toBeGreaterThan(0);
    expect(within(article).queryByText(/outra spec/)).toBeNull();
    expect(within(article).getByRole("button", { name: "Retomar manualmente" })).toBeTruthy();
  });

  it("escalada completa mostra os blocos estruturados, com o trecho do erro em texto", async () => {
    await open(stateWith(pausedSpec()));
    const full = [
      "Análise de recuperação #3 (ESCALATE)",
      "- Causa raiz: porta ocupada",
      "- Decisão: escalar ao humano",
      "- Contexto:",
      "  - comando: cargo test",
      "  - código: 101",
      "  - trecho do erro:",
      "      error: boom",
      "        at <b>x</b>",
      "  - o que foi tentado: 3 de 3 ciclos de recuperação e 2 de 2 rodadas autônomas",
      "  - por que não resolveu: o erro persistiu",
      "  - opções: (1) liberar a porta; (2) trocar a porta; (3) abortar",
    ].join("\n");
    applyEvent("agent_message", { agent: "architect", text: full, task_id: "T01", spec_id: "s1", seq: 1 });
    applyEvent("recovery_decided", { spec_id: "s1", task_id: "T01", cycle: 1, decision: "ESCALATE" });
    applyEvent("project_status", { status: "blocked", task_id: "T01", reason: "r", kind: "quality" });
    const article = card();
    expect(within(article).getByText("O que aconteceu")).toBeTruthy();
    expect(within(article).getAllByText("porta ocupada").length).toBeGreaterThan(0);
    expect(within(article).getAllByText("cargo test").length).toBeGreaterThan(0);
    expect(within(article).getByText("O que já foi tentado")).toBeTruthy();
    expect(within(article).getByText("Por que parou")).toBeTruthy();
    expect(within(article).getByText("O que você pode fazer")).toBeTruthy();
    expect(within(article).getAllByRole("listitem").some((li) => li.textContent === "trocar a porta")).toBe(true);
    const pre = article.querySelector("details pre");
    expect(pre?.textContent).toContain("at <b>x</b>");
    expect(pre?.querySelector("b")).toBeNull();
  });

  // Spec 124 T03: o ASK pausa com a pergunta do Arquiteto em destaque e as opções clicáveis.
  it("ASK mostra a pergunta do Arquiteto, as opções e a orientação livre", async () => {
    await open(stateWith(pausedSpec()));
    const ask = [
      "Análise de recuperação #2 (ASK)",
      "- Causa raiz: o critério das capturas conflita com o tempo da bateria",
      "- Decisão: pergunta ao humano (ASK)",
      "- Pergunta ao humano: As capturas em 1920x1080 podem ficar como verificação manual?",
      "- Opções para o humano:",
      "  - [guidance] manual: Sim, manual",
      "- Contexto:",
      "  - opções: escolha uma das opções propostas acima, escreva uma orientação própria ou use Retomar",
    ].join("\n");
    const options = [{ id: "manual", label: "Sim, manual", kind: "guidance", text: "capturas viram verificação manual" }];
    applyEvent("agent_message", { agent: "architect", text: ask, task_id: "T01", spec_id: "s1", seq: 1 });
    applyEvent("recovery_decided", { spec_id: "s1", task_id: "T01", cycle: 2, decision: "ASK", options, options_analysis: 2 });
    applyEvent("project_status", { status: "blocked", task_id: "T01", reason: "r", kind: "quality" });
    const article = card();
    expect(within(article).getByText("Pergunta do Arquiteto")).toBeTruthy();
    const question = article.querySelector("[data-recovery-question]");
    expect(question?.textContent).toBe("As capturas em 1920x1080 podem ficar como verificação manual?");
    expect(within(article).getByRole("button", { name: "Escolher: Sim, manual" })).toBeTruthy();
    expect(findForbiddenGlyphs(article)).toEqual([]);
  });

  it("quality com ESCALATE de outra spec mantém o cartão de retomada comum", async () => {
    await open(stateWith(pausedSpec()));
    applyEvent("recovery_decided", { spec_id: "s2", task_id: "T01", cycle: 1, decision: "ESCALATE" });
    applyEvent("project_status", { status: "blocked", task_id: "T01", reason: "r", kind: "quality" });
    expect(within(card()).getByRole("button", { name: "Retomar" })).toBeTruthy();
    expect(within(card()).queryByText("O Arquiteto analisou e escalou para você")).toBeNull();
  });

  it("environmental: título e instrução próprios, sem a análise do Arquiteto", async () => {
    await open(stateWith(makeSpec("s1", "Plano", "IN_PROGRESS", [makeTask("T01", "A", "BLOCKED", { next_action: "motivo do bloqueio" })])));
    applyEvent("project_status", { status: "blocked", task_id: "T01", reason: "r", kind: "environmental" });
    const article = card();
    expect(within(article).getByText("Bloqueio de ambiente")).toBeTruthy();
    expect(within(article).getByText("Resolva o problema indicado e retome.")).toBeTruthy();
    expect(within(article).getAllByText("motivo do bloqueio").length).toBeGreaterThan(0);
    expect(within(article).queryByText("O Arquiteto analisou e escalou para você")).toBeNull();
  });

  it("sem natureza informada: 'Execução pausada' com Retomar", async () => {
    await open(stateWith(pausedSpec()));
    applyEvent("project_status", { status: "blocked", task_id: "T01", reason: "r" });
    expect(within(card()).getByText("Execução pausada")).toBeTruthy();
    expect(within(card()).getByRole("button", { name: "Retomar" })).toBeTruthy();
  });

  it("opções vindas do evento recovery_decided são usadas", async () => {
    const { server } = await open(stateWith(pausedSpec()));
    server.reply("POST", CHOICE, () => ({ status: 202, body: { ok: true } }));
    applyEvent("recovery_decided", { spec_id: "s1", task_id: "T01", cycle: 1, decision: "ESCALATE", options: OPTIONS, options_analysis: 9 });
    applyEvent("project_status", { status: "blocked", task_id: "T01", reason: "r", kind: "quality" });
    await chooseOption("Usar npx tsc");
    expect(choiceBodies(server)).toEqual([{ option_id: "o1", analysis: 9 }]);
  });

  it("análise com 'Contagem' não mostra a linha no cartão nem a junta à última opção", async () => {
    await open(stateWith(pausedSpec()));
    const text = ["Análise de recuperação #3 (ESCALATE)", "- Causa raiz: porta ocupada", "- Contexto:", "  - opções: (1) liberar a porta; (2) abortar", "- Contagem: não conta como ciclo (escalada determinística)"].join("\n");
    applyEvent("agent_message", { agent: "architect", text, task_id: "T01", spec_id: "s1", seq: 1 });
    expect(within(card()).getByText("abortar")).toBeTruthy();
    expect(within(card()).queryByText(/Contagem/)).toBeNull();
  });

  it("opções antes/depois em texto (sem HTML); cancelar a confirmação não chama o endpoint", async () => {
    const { server } = await open(stateWith(withOptions()));
    const pre = card().querySelector("pre");
    expect(pre?.textContent).toContain("Antes:\ntsc");
    expect(pre?.textContent).toContain("<b>x</b>");
    expect(pre?.querySelector("b")).toBeNull();
    expect(within(card()).getByText("Só a API")).toBeTruthy();
    fireEvent.click(within(card()).getByRole("button", { name: "Escolher: Usar npx tsc" }));
    fireEvent.click(within(card()).getByRole("button", { name: "Cancelar" }));
    expect(within(card()).queryByRole("button", { name: RECOVERY_CONFIRM })).toBeNull();
    expect(server.count("POST", CHOICE)).toBe(0);
  });
});
