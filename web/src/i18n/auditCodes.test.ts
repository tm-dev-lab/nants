// @vitest-environment jsdom
/*
 * Pendências da auditoria dos idiomas (spec 102 T06): os textos fixos do runner e do servidor que
 * ainda saíam só em pt-BR passam a vir com `code`/`params` e aparecem no idioma ativo.
 * - etapas da atividade (testes, checagens, relatório, ARCHITECTURE.md, revisão do plano);
 * - rótulos das ferramentas e avisos do limitador, ao vivo, no instantâneo e na trilha;
 * - avisos da verificação final; pasta aninhada; anexos; "falha interna";
 * - o `report_in_progress` do App e as pendências do 412 refeitas no idioma atual;
 * - a opção "ambiental" da análise de recuperação, conferida contra o texto do servidor.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { getConfig, getProjectActivity, getToolActions, uploadChatAttachment } from "../api/client";
import { parseApiEvent } from "../api/events";
import type { ApiEvent } from "../api/types";
import { runProblemText } from "../apps/app/runSession";
import { stageText, stepText, toolActionText, toolText } from "../apps/shared/activityText";
import { appErrorMessage } from "../copy";
import { localizeRecoveryAnalysis } from "../recoveryAnalysis";
import { createAppStore } from "../store";
import { en } from "./en";
import { setLanguage } from "./index";
import { ptBR } from "./ptBR";
import { looksPortuguese } from "./ptLeftovers";
import { apiFailureText, systemMessageText } from "./serverCodes";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

function stubFetch(status: number, body: unknown) {
  vi.stubGlobal("fetch", vi.fn(async () => jsonResponse(status, body)));
}

afterEach(() => {
  vi.unstubAllGlobals();
  setLanguage("pt-BR", { persist: false, storage: null });
});

/** Códigos que o servidor passou a mandar na T06, por família (cada um com chave nos dois idiomas). */
const ACTIVITY_STAGE_CODES = ["report", "architecture", "plan_review", "final_verification", "tests", "tests_base", "check", "check_base"];
const ACTIVITY_CODES = [
  "tool.read_file", "tool.list_dir", "tool.search_code", "tool.write_file", "tool.write_sdd_artifacts", "tool.run_command",
  "tool.fetch_url", "tool.package_versions", "tool.web_source", "tool.web_search_unavailable", "tool.view_reference",
  "tool.view_screenshot", "tool.images_refused", "tool.generate_image", "tool.image_failed", "tool.other",
  "pace.waiting", "pace.throttled", "pace.queued", "plan.checking",
];
const SYSTEM_CODES = ["final.skipped", "final.unavailable", "final.still_unmet", "final.fix_refused", "final.fix_created", "task.worktree_dirty", "task.ownership_failed", "task.cleanup_failed"];

describe("códigos novos do servidor (spec 102 T06)", () => {
  it("cada code tem texto nos dois idiomas, e o en não tem português", () => {
    const keys = [
      ...ACTIVITY_STAGE_CODES.map((c) => `activity.stage.${c}`),
      ...ACTIVITY_CODES.map((c) => `activity.code.${c}`),
      ...SYSTEM_CODES.map((c) => `system.${c}`),
      "serverError.project_nested_repo",
      "app.error.report_in_progress",
    ];
    for (const key of keys) {
      expect(Object.keys(ptBR), key).toContain(key);
      expect(looksPortuguese(en[key as keyof typeof en]), key).toBe(false);
    }
  });

  it("os codes do Rust estão no dicionário (lidos do código do servidor)", () => {
    const root = resolve(process.cwd(), "..");
    const activity = readFileSync(resolve(root, "src/agents/activity.rs"), "utf8");
    const fromRust = [...activity.matchAll(/c\("(tool\.[a-z_]+)"|ActivityCode::new\("([a-z_.]+)"/g)].map((m) => m[1] ?? m[2] ?? "");
    expect(fromRust.length).toBeGreaterThan(15);
    for (const code of fromRust) expect(ACTIVITY_CODES, code).toContain(code);
    const runner = readFileSync(resolve(root, "src/agents/runner.rs"), "utf8");
    for (const m of runner.matchAll(/"(final\.[a-z_]+)"/g)) expect(SYSTEM_CODES).toContain(m[1]);
    for (const m of runner.matchAll(/=> "((?:tests|check)(?:_base)?)",/g)) expect(ACTIVITY_STAGE_CODES).toContain(m[1]);
  });
});

describe("atividade ao vivo em en pelo code", () => {
  function activity(payload: Record<string, unknown>): ApiEvent {
    const ev = parseApiEvent({ event_type: "agent_activity", project_id: "p1", payload: { agent: "developer", task_id: "T01", activity_id: "a1", ...payload } });
    if (ev === null) throw new Error("evento inválido");
    return ev;
  }

  it("etapa, rótulo e aviso do evento saem no idioma ativo; sem code, como vieram", () => {
    setLanguage("en", { persist: false, storage: null });
    const store = createAppStore();
    store.applyEvent(activity({ kind: "start", stage: "Testes · tentativa 2", stage_code: "tests", stage_params: { attempt: 2 } }));
    store.applyEvent(activity({ kind: "tool", text: "lendo src/a.rs", code: "tool.read_file", params: { path: "src/a.rs" } }));
    store.applyEvent(activity({ kind: "text", text: "ajustando o ritmo ao limite do provedor: 3 s", code: "pace.waiting", params: { seconds: "3" } }));
    const [entry] = store.getState().activities.p1 ?? [];
    if (entry === undefined) throw new Error("atividade ausente");
    expect(stageText(entry)).toBe("Tests · attempt 2");
    expect(toolText(entry)).toBe("reading src/a.rs");
    expect(stepText(entry)).toBe("adjusting the pace to the provider limit: 3 s");
    // Texto do agente sem code limpa o aviso; rótulo com code inválido também.
    store.applyEvent(activity({ kind: "text", text: "Hello" }));
    store.applyEvent(activity({ kind: "tool", text: "lendo b", code: "Tool!" }));
    const [after] = store.getState().activities.p1 ?? [];
    if (after === undefined) throw new Error("atividade ausente");
    expect(stepText(after)).toBe("Hello");
    expect(toolText(after)).toBe("lendo b");
    // Etapa antiga, sem code.
    expect(stageText({ stage: "Escrevendo relatório final" })).toBe("Escrevendo relatório final");
    expect(stageText({ stage: "final_verification" })).toBe("Final verification in progress");
    // Nova tentativa: em en só a contagem; em pt-BR, como veio.
    const retry = { text: "resposta do provedor falhou (incompleta: x); repetindo (1/2)" };
    expect(stepText(retry)).toBe("attempt 1 of 2");
    setLanguage("pt-BR", { persist: false, storage: null });
    expect(stepText(retry)).toBe(retry.text);
  });

  it("instantâneo e trilha guardam os codes; a linha antiga aparece como veio", async () => {
    setLanguage("en", { persist: false, storage: null });
    stubFetch(200, {
      activities: [
        {
          agent: "architect",
          task_id: null,
          spec_id: "s1",
          activity_id: "a9",
          reasoning: "",
          text: "aguardando o ritmo",
          tool: "gerando imagem web/a.png",
          stage: "Escrevendo relatório final",
          stage_code: "report",
          tool_code: "tool.generate_image",
          tool_params: { path: "web/a.png" },
          text_code: "pace.queued",
          text_params: {},
          age_ms: 10,
        },
      ],
    });
    const snap = await getProjectActivity("p1");
    if (!snap.ok) throw new Error("esperava sucesso");
    const store = createAppStore();
    store.mergeActivities("p1", snap.data, Date.now());
    const [entry] = store.getState().activities.p1 ?? [];
    if (entry === undefined) throw new Error("atividade ausente");
    expect(stageText(entry)).toBe("Writing the final report");
    expect(toolText(entry)).toBe("generating image web/a.png");
    expect(stepText(entry)).toBe("waiting for the pace");

    stubFetch(200, {
      items: [
        { seq: 1, ts: 1, agent: "developer", task_id: "T01", kind: "tool", text: "lendo x", code: "tool.read_file", params: { path: "x" }, stage_code: "report" },
        { seq: 2, ts: 2, agent: "developer", task_id: "T01", kind: "tool", text: "gerando imagem antiga.png" },
      ],
      total: 2,
      has_more: false,
    });
    const trail = await getToolActions("p1", "s1");
    if (!trail.ok) throw new Error("esperava sucesso");
    expect(trail.data.items.map(toolActionText)).toEqual(["reading x", "gerando imagem antiga.png"]);
    expect(trail.data.items[0]?.stage_code).toBe("report");
  });
});

describe("mensagens fixas do servidor no idioma ativo", () => {
  it("avisos da verificação final pelo code", () => {
    // Spec 107 T01: o título é o gravado na tarefa (conteúdo do projeto), sem tradução.
    const created = { text: "Verificação final: criada a tarefa T05 (Final verification fix) para os critérios não atendidos.", code: "final.fix_created", params: { task: "T05", title: "Final verification fix" } };
    expect(systemMessageText(created, "en")).toBe("Final verification: created task T05 (Final verification fix) for the unmet criteria.");
    const createdPt = { ...created, params: { task: "T05", title: "Correção da verificação final" } };
    expect(systemMessageText(createdPt, "pt-BR")).toBe("Verificação final: criada a tarefa T05 (Correção da verificação final) para os critérios não atendidos.");
    expect(systemMessageText(createdPt, "en")).toBe("Final verification: created task T05 (Correção da verificação final) for the unmet criteria.");
    // Aviso antigo, sem o título nos params: fica o texto gravado.
    const legacy = { text: "Verificação final: criada a tarefa T05 (Correção da verificação final) para os critérios não atendidos.", code: "final.fix_created", params: { task: "T05" } };
    expect(systemMessageText(legacy, "en")).toBe(legacy.text);
    // Lista longa (acima dos 512 bytes de um param): vem inteira do texto gravado, nos dois idiomas.
    const list = Array.from({ length: 30 }, (_, i) => `- G${i + 1}: critério ${i + 1} sem evidência na base integrada`).join("\n");
    expect(new TextEncoder().encode(list).length).toBeGreaterThan(512);
    const refused = { text: `${ptBR["system.final.fix_refused"]}\n${list}`, code: "final.fix_refused", params: {} };
    expect(systemMessageText(refused, "en")).toBe(`Warning: fix task refused; the spec will be completed with unmet criteria.\n${list}`);
    expect(systemMessageText(refused, "pt-BR")).toBe(refused.text);
    const unmet = { text: `${ptBR["system.final.still_unmet"]}\n${list}`, code: "final.still_unmet", params: {} };
    expect(systemMessageText(unmet, "en")).toBe(`${en["system.final.still_unmet"]}\n${list}`);
    expect(systemMessageText(unmet, "pt-BR")).toBe(unmet.text);
    const error = "x".repeat(700);
    const unavailable = { text: `${ptBR["system.final.unavailable"]}\n${error}`, code: "final.unavailable", params: {} };
    expect(systemMessageText(unavailable, "en")).toBe(`Final verification unavailable; completing without it.\n${error}`);
    expect(systemMessageText(unavailable, "pt-BR")).toBe(unavailable.text);
    expect(systemMessageText({ text: "AVISO", code: "final.skipped", params: { reason: "orçamento" } }, "en")).not.toMatch(/orçamento/);
  });

  it("'falha interna' e 'projeto não encontrado' dos arquivos e do App saem em en", async () => {
    stubFetch(500, { error: "falha interna" });
    const internal = await getConfig();
    if (internal.ok) throw new Error("esperava falha");
    expect(apiFailureText(internal.error, "en")).toBe("internal failure");
    stubFetch(404, { error: "projeto não encontrado" });
    const missing = await getConfig();
    if (missing.ok) throw new Error("esperava falha");
    expect(apiFailureText(missing.error, "en")).toBe("project not found");
  });

  it("pasta dentro de outro repositório pelo code, com o caminho", async () => {
    stubFetch(409, { error: "a pasta está dentro do repositório Git /home/x; escolha...", code: "project_nested_repo", params: { path: "/home/x" } });
    const r = await getConfig();
    if (r.ok) throw new Error("esperava falha");
    expect(apiFailureText(r.error, "en")).toBe("The folder is inside the Git repository /home/x; choose the repository root or another folder.");
  });

  it("falhas do envio de imagem saem no idioma ativo", async () => {
    for (const [status, text] of [
      [413, "image larger than 5 MiB"],
      [415, "unsupported image format (use PNG, JPEG or WebP)"],
      [507, "no space for images in this project (200 MiB limit)"],
    ] as const) {
      stubFetch(status, {});
      const r = await uploadChatAttachment("p1", new Blob(["x"]), "image/png");
      if (r.ok) throw new Error("esperava falha");
      expect(apiFailureText(r.error, "en")).toBe(text);
    }
  });
});

describe("App: relatório em curso e pendências do 412 no idioma atual", () => {
  it("report_in_progress tem texto próprio", () => {
    setLanguage("en", { persist: false, storage: null });
    expect(appErrorMessage(409, "report_in_progress")).toBe("The Architect's final report is being generated; try again in a moment.");
    setLanguage("pt-BR", { persist: false, storage: null });
    expect(appErrorMessage(409, "report_in_progress")).toBe(ptBR["app.error.report_in_progress"]);
  });

  it("as pendências do 412 são refeitas no idioma atual (a troca vale sem novo envio)", () => {
    const problem = {
      message: "Falta configurar: Docker.",
      missing: [{ id: "docker", label: "Docker", settings_tab: "machine" as const }],
      prereq: true,
    };
    setLanguage("en", { persist: false, storage: null });
    const english = runProblemText(problem);
    expect(english).not.toBe(problem.message);
    expect(looksPortuguese(english)).toBe(false);
    setLanguage("pt-BR", { persist: false, storage: null });
    expect(runProblemText(problem)).toMatch(/^Falta configurar/);
  });
});

describe("análise de recuperação: opções fixas do servidor em en", () => {
  /** Literal de opções do `recovery.rs`, com a continuação de linha do Rust (`\` + quebra + recuo some). */
  function rustOptions(): string[] {
    const source = readFileSync(resolve(process.cwd(), "..", "src/server/recovery.rs"), "utf8");
    return [...source.matchAll(/"(  - opções: (?:[^"\\]|\\[\s\S])*)"/g)].map((m) => (m[1] ?? "").replace(/\\\n\s*/g, ""));
  }

  it("as três opções do servidor (inclusive a ambiental, em várias linhas no código) saem traduzidas", () => {
    const options = rustOptions();
    expect(options.length).toBe(3);
    const environmental = options.find((o) => o.includes("ambiente docker"));
    expect(environmental).toBeDefined();
    for (const option of options) {
      const text = `Análise de recuperação #2 (ESCALATE)\n- Causa raiz: ambiente: o comando de testes não executou\n- Contexto:\n${option}`;
      const shown = localizeRecoveryAnalysis(text, "en");
      const last = shown.split("\n").at(-1) ?? "";
      expect(last.startsWith("  - options: "), last).toBe(true);
      expect(looksPortuguese(last), last).toBe(false);
    }
    const shown = localizeRecoveryAnalysis(`Análise de recuperação #1 (ESCALATE)\n${environmental ?? ""}`, "en");
    expect(shown).toContain(en["recoveryText.options.environmental"]);
  });
});
