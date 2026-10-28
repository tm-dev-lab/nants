/* Mensagem do Revisor na janela Conversa (spec 093, T06): selo do veredito, apontamentos e matriz de critérios. */
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
import { Conversation } from "./Conversation";
import { parseReviewMessage, splitChecklist } from "../../reviewMessage";
import type { SpecView } from "../../api/types";
import type { ChatMessage } from "../../store";
import { makeTask, resetStore } from "../../test-utils";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { setLanguage } from "../../i18n";

afterEach(() => {
  cleanup();
  resetStore();
});

function view(spec: SpecView, messages: ChatMessage[]) {
  return render(<Conversation projectId="p1" spec={spec} messages={messages} onDraftUpdated={async () => undefined} />);
}

const TEXT = "Reprovado.\n\nCritérios:\n✓ A — src/a.rs:1\n✗ B — <b>x</b>";

describe("checklist do Revisor", () => {
  it("separa resumo e itens", () => {
    const r = splitChecklist(TEXT);
    expect(r?.summary).toBe("Reprovado.");
    expect(r?.items).toEqual([
      { met: true, text: "A — src/a.rs:1" },
      { met: false, text: "B — <b>x</b>" },
    ]);
    expect(splitChecklist("só texto")).toBeNull();
  });

  it("renderiza ✓/✗ com evidência como texto puro na mensagem do Revisor", () => {
    const message = { id: 1, role: "agent", agent: "reviewer", text: TEXT, at: 1, origin: "live", seq: 1, taskId: "T02", attempt: 1 } as ChatMessage;
    const spec: SpecView = { id: "s1", title: "Plano", status: "IN_PROGRESS", tasks: [makeTask("T02", "X", "IN_PROGRESS")] };
    const { container } = view(spec, [message]);
    const list = screen.getByRole("list", { name: "Critérios de aceitação" });
    const items = within(list).getAllByRole("listitem");
    expect(items).toHaveLength(2);
    expect(items[0]?.textContent).toContain("A — src/a.rs:1");
    expect(items[1]?.textContent).toContain("<b>x</b>");
    expect(container.querySelector("b")).toBeNull();
    // As marcas viram ícones com nome acessível (sem caractere).
    expect(within(items[0] as HTMLElement).getByRole("img", { name: "atendido" }).querySelector("svg")).not.toBeNull();
    expect(within(items[1] as HTMLElement).getByRole("img", { name: "não atendido" })).toBeTruthy();
    expect(findForbiddenGlyphs(container)).toEqual([]);
  });

  it("mostra o critério não verificado (spec 133 T06) com marca própria, distinta de não atendido", () => {
    const text = "Aprovado: ok\n\nCritérios:\n✓ A: src/a.rs:1\n? B: não verificado: a evidência cita o que não existe (x.js:9)";
    expect(splitChecklist(text)?.items).toEqual([
      { met: true, text: "A: src/a.rs:1" },
      { met: false, unverified: true, text: "B: não verificado: a evidência cita o que não existe (x.js:9)" },
    ]);
    const { container } = renderMessage(text);
    const items = within(screen.getByRole("list", { name: "Critérios de aceitação" })).getAllByRole("listitem");
    expect(items[1]?.getAttribute("data-met")).toBe("unverified");
    expect(within(items[1] as HTMLElement).getByRole("img", { name: "não verificado" })).toBeTruthy();
    expect(within(items[1] as HTMLElement).queryByRole("img", { name: "não atendido" })).toBeNull();
    expect(findForbiddenGlyphs(container)).toEqual([]);
    cleanup();
    setLanguage("en", { persist: false });
    try {
      renderMessage(text.replace("Aprovado", "Approved").replace("Critérios", "Criteria"));
      const enItems = within(screen.getByRole("list", { name: "Acceptance criteria" })).getAllByRole("listitem");
      expect(within(enItems[1] as HTMLElement).getByRole("img", { name: "not verified" })).toBeTruthy();
    } finally {
      setLanguage("pt-BR", { persist: false });
    }
  });
});

const FULL =
  "Pediu ajustes — falta tratar o erro\n\nApontamentos:\n" +
  "- F-01 [MEDIUM] src/main.jsx:62-65 — descrição <b>x</b> → trocar por Y\n" +
  "- F-03 [HIGH] src/a.rs:1 — sem recomendação\n\n" +
  "Observações (não bloqueiam):\n- F-02 [LOW] src/b.rs:9 — detalhe menor\n\n" +
  "Critérios:\n✓ A — ok\n✗ B — falhou";

function reviewerMessage(text: string): ChatMessage {
  return { id: 1, role: "agent", agent: "reviewer", text, at: 1, origin: "live", seq: 1, taskId: "T02", attempt: 1 } as ChatMessage;
}

function renderMessage(text: string) {
  const spec: SpecView = { id: "s1", title: "Plano", status: "IN_PROGRESS", tasks: [makeTask("T02", "X", "IN_PROGRESS")] };
  return view(spec, [reviewerMessage(text)]);
}

describe("parseReviewMessage", () => {
  it("lê veredito, resumo, apontamentos, observações e critérios", () => {
    const r = parseReviewMessage(FULL);
    expect(r?.verdict).toBe("changes");
    expect(r?.prefix).toBe("");
    expect(r?.summary).toBe("falta tratar o erro");
    expect(r?.findings).toEqual([
      { id: "F-01", severity: "MEDIUM", location: "src/main.jsx:62-65", description: "descrição <b>x</b>", recommendation: "trocar por Y" },
      { id: "F-03", severity: "HIGH", location: "src/a.rs:1", description: "sem recomendação", recommendation: "" },
    ]);
    expect(r?.observations).toEqual([
      { id: "F-02", severity: "LOW", location: "src/b.rs:9", description: "detalhe menor", recommendation: "" },
    ]);
    expect(r?.items).toEqual([
      { met: true, text: "A — ok" },
      { met: false, text: "B — falhou" },
    ]);
  });

  it("omite seções vazias e aceita só o veredito", () => {
    const r = parseReviewMessage("Aprovado — tudo certo");
    expect(r).toEqual({ prefix: "", verdict: "approved", summary: "tudo certo", findings: [], observations: [], resolved: [], responses: [], decisions: [], items: [], captures: [] });
    const onlyObs = parseReviewMessage("Aprovado — ok\n\nObservações (não bloqueiam):\n- F-02 [LOW] x.rs:1 — d\n\nCritérios:\n✓ A — e");
    expect(onlyObs?.findings).toEqual([]);
    expect(onlyObs?.observations).toHaveLength(1);
    expect(onlyObs?.items).toHaveLength(1);
  });

  it("verificação final: o veredito vem depois da linha de contagem", () => {
    const r = parseReviewMessage("Verificação final: 1 de 2 critérios atendidos\n\nPediu ajustes — x\n\nApontamentos:\n- F-01 [HIGH] a:1 — d → r\n\nCritérios:\n✓ A — e\n✗ B — f");
    expect(r?.prefix).toBe("Verificação final: 1 de 2 critérios atendidos");
    expect(r?.verdict).toBe("changes");
    expect(r?.summary).toBe("x");
    expect(r?.findings).toHaveLength(1);
    expect(r?.items).toHaveLength(2);
  });

  it("mensagem antiga: sem veredito segue como antes; texto comum é null", () => {
    const old = parseReviewMessage(TEXT);
    expect(old?.verdict).toBeNull();
    expect(old?.summary).toBe("Reprovado.");
    expect(old?.items).toHaveLength(2);
    expect(parseReviewMessage("só texto")).toBeNull();
    expect(parseReviewMessage("Reprovado. sem matriz")).toBeNull();
  });

  it("formato novo (095/T08): dois-pontos no lugar do travessão; o primeiro separador vale", () => {
    const r = parseReviewMessage(
      "Pediu ajustes: falta o teste\n\nApontamentos:\n- F-01 [HIGH] src/a.rs:10: índice — sem checagem → validar\n" +
        "- F-02 [MEDIUM] : sem local\n\nObservações (não bloqueiam):\n- F-03 [MEDIUM] src/b.rs:3: nome: confuso\n\nCritérios:\n✓ A: src/a.rs:1",
    );
    expect(r?.verdict).toBe("changes");
    expect(r?.summary).toBe("falta o teste");
    expect(r?.findings[0]).toEqual({ id: "F-01", severity: "HIGH", location: "src/a.rs:10", description: "índice — sem checagem", recommendation: "validar" });
    expect(r?.findings[1]?.location).toBe("");
    expect(r?.findings[1]?.description).toBe("sem local");
    expect(r?.observations[0]).toMatchObject({ location: "src/b.rs:3", description: "nome: confuso" });
    expect(r?.items).toEqual([{ met: true, text: "A: src/a.rs:1" }]);
    expect(parseReviewMessage("Aprovado: ok")?.verdict).toBe("approved");
    // Mensagem antiga com travessão, cuja descrição tem dois-pontos: o travessão vem antes e vale.
    expect(parseReviewMessage("Aprovado — ok\n\nObservações (não bloqueiam):\n- F-02 [LOW] x.rs:1 — a: b")?.observations[0]).toMatchObject({ location: "x.rs:1", description: "a: b" });
  });

  it("linha de finding fora do formato vira só descrição", () => {
    const r = parseReviewMessage("Pediu ajustes — x\n\nApontamentos:\n- algo solto");
    expect(r?.findings).toEqual([{ id: "", severity: "", location: "", description: "algo solto", recommendation: "" }]);
  });
});

describe("renderização da mensagem do Revisor", () => {
  it("mostra selo âmbar, apontamentos, observações e critérios em texto puro", () => {
    const { container } = renderMessage(FULL);
    const badge = screen.getByTestId("review-verdict");
    expect(badge.textContent).toContain("Pediu ajustes");
    expect(badge.getAttribute("data-verdict")).toBe("changes");
    expect(badge.getAttribute("data-tone")).toBe("amber");
    const findings = within(screen.getByRole("list", { name: "Apontamentos" })).getAllByRole("listitem");
    expect(findings).toHaveLength(2);
    expect(findings[0]?.textContent).toContain("F-01");
    expect(findings[0]?.textContent).toContain("MEDIUM");
    expect(findings[0]?.textContent).toContain("src/main.jsx:62-65");
    expect(findings[0]?.textContent).toContain("descrição <b>x</b>");
    expect(findings[0]?.textContent).toContain("trocar por Y");
    // A seta da recomendação é ícone, não caractere.
    expect(findings[0]?.querySelector("svg[data-icon='arrow-right']")).not.toBeNull();
    expect(findForbiddenGlyphs(container)).toEqual([]);
    const obs = within(screen.getByRole("list", { name: "Observações" })).getAllByRole("listitem");
    expect(obs).toHaveLength(1);
    expect(obs[0]?.textContent).toContain("detalhe menor");
    expect(within(screen.getByRole("list", { name: "Critérios de aceitação" })).getAllByRole("listitem")).toHaveLength(2);
    expect(container.querySelector("b")).toBeNull();
  });

  it("selo verde quando aprovado e sem listas vazias", () => {
    renderMessage("Aprovado — tudo certo");
    const badge = screen.getByTestId("review-verdict");
    expect(badge.textContent).toContain("Aprovado");
    expect(badge.getAttribute("data-tone")).toBe("green");
    expect(screen.queryByRole("list", { name: "Apontamentos" })).toBeNull();
    expect(screen.queryByRole("list", { name: "Observações" })).toBeNull();
    expect(screen.queryByRole("list", { name: "Critérios de aceitação" })).toBeNull();
  });

  it("verificação final mostra a contagem e o selo", () => {
    renderMessage("Verificação final: 2 de 2 critérios atendidos\n\nAprovado — ok\n\nCritérios:\n✓ A — e\n✓ B — f");
    expect(screen.getByText("Verificação final: 2 de 2 critérios atendidos")).toBeTruthy();
    expect(screen.getByTestId("review-verdict").textContent).toContain("Aprovado");
  });

  it("mensagem antiga não mostra selo", () => {
    renderMessage(TEXT);
    expect(screen.queryByTestId("review-verdict")).toBeNull();
    expect(screen.getByRole("list", { name: "Critérios de aceitação" })).toBeTruthy();
  });
});

describe("cenário de falha (spec 087 T05)", () => {
  const WITH_SCENARIO =
    "Pediu ajustes — r\n\nApontamentos:\n" +
    "- F-01 [HIGH] src/a.rs:1 — índice sem checagem → validar\n  Cenário de falha: lista vazia → <i>pânico</i>\n" +
    "- F-02 [MEDIUM] src/b.rs:2 — detalhe\n\n" +
    "Observações (não bloqueiam):\n- F-03 [HIGH, baixa confiança] src/c.rs:3 — talvez\n  Cenário de falha: x → y";

  it("lê o cenário abaixo do finding e deixa os demais sem ele", () => {
    const r = parseReviewMessage(WITH_SCENARIO);
    expect(r?.findings[0]).toEqual({
      id: "F-01",
      severity: "HIGH",
      location: "src/a.rs:1",
      description: "índice sem checagem",
      recommendation: "validar",
      scenario: "lista vazia → <i>pânico</i>",
    });
    expect(r?.findings[1]?.scenario).toBeUndefined();
    expect(r?.observations[0]?.severity).toBe("HIGH");
    expect(r?.observations[0]?.lowConfidence).toBe(true);
    expect(r?.observations[0]?.scenario).toBe("x → y");
  });

  it("mostra o cenário numa linha discreta, em texto puro", () => {
    const { container } = renderMessage(WITH_SCENARIO);
    const lines = screen.getAllByTestId("review-scenario");
    expect(lines).toHaveLength(2);
    expect(lines[0]?.textContent).toBe("Cenário de falha: lista vazia → <i>pânico</i>");
    expect(container.querySelector("i")).toBeNull();
  });

  it("mensagem antiga, sem cenário, segue igual", () => {
    renderMessage(FULL);
    expect(screen.queryByTestId("review-scenario")).toBeNull();
    expect(parseReviewMessage(FULL)?.findings.every((f) => f.scenario === undefined)).toBe(true);
  });
});

describe("observação de arquivo inalterado (spec 092 T04)", () => {
  const TEXT_UNCHANGED =
    "Aprovado — ok (findings novos em arquivos que não mudaram desde a revisão anterior viraram observações.)\n\n" +
    "Observações (não bloqueiam):\n- F-07 [MEDIUM, observação (arquivo não mudou nesta rodada)] src/b.rs:3 — nome confuso\n" +
    "- F-08 [MEDIUM] src/c.rs:1 — outro";

  it("separa o rótulo da severidade", () => {
    const r = parseReviewMessage(TEXT_UNCHANGED);
    expect(r?.observations[0]).toEqual({
      id: "F-07",
      severity: "MEDIUM",
      location: "src/b.rs:3",
      description: "nome confuso",
      recommendation: "",
      unchangedFile: true,
    });
    expect(r?.observations[1]?.unchangedFile).toBeUndefined();
  });

  it("reconhece o rótulo seguido de baixa confiança", () => {
    const text =
      "Aprovado — ok\n\nObservações (não bloqueiam):\n" +
      "- F-09 [MEDIUM, observação (arquivo não mudou nesta rodada), baixa confiança] src/d.rs:4 — talvez";
    const r = parseReviewMessage(text);
    expect(r?.observations[0]?.unchangedFile).toBe(true);
    expect(r?.observations[0]?.lowConfidence).toBe(true);
    expect(r?.observations[0]?.severity).toBe("MEDIUM");
    renderMessage(text);
    expect(screen.getByTestId("review-unchanged-file").textContent).toBe("observação (arquivo não mudou nesta rodada)");
    expect(screen.getByTestId("review-low-confidence").textContent).toBe("baixa confiança");
  });

  it("baixa confiança sozinha vira selo no idioma ativo (spec 115 T03)", () => {
    const text = "Aprovado: ok\n\nObservações (não bloqueiam):\n- F-02 [HIGH, baixa confiança] src/lib.rs:1: descrição";
    const r = parseReviewMessage(text);
    expect(r?.observations[0]).toEqual({
      id: "F-02",
      severity: "HIGH",
      location: "src/lib.rs:1",
      description: "descrição",
      recommendation: "",
      lowConfidence: true,
    });
    setLanguage("en", { persist: false });
    try {
      renderMessage(text);
      expect(screen.getByTestId("review-low-confidence").textContent).toBe("low confidence");
      expect(screen.queryByText(/baixa confiança/)).toBeNull();
    } finally {
      setLanguage("pt-BR", { persist: false });
    }
  });

  it("mostra o rótulo só na observação rebaixada", () => {
    renderMessage(TEXT_UNCHANGED);
    const labels = screen.getAllByTestId("review-unchanged-file");
    expect(labels).toHaveLength(1);
    expect(labels[0]?.textContent).toBe("observação (arquivo não mudou nesta rodada)");
    const items = within(screen.getByRole("list", { name: "Observações" })).getAllByRole("listitem");
    expect(items[0]?.textContent).toContain("observação (arquivo não mudou nesta rodada)");
    expect(items[1]?.textContent).not.toContain("arquivo não mudou");
  });
});

/* Spec 124 T01: estado dos apontamentos (formato de `render_review_message_tracked`). */
const TRACKED =
  "Pediu ajustes: falta pouco\n\nApontamentos:\n" +
  "- F-02 [HIGH, aberto há 3 revisões] src/b.rs:9: sem teste → adicionar\n" +
  "- F-04 [MEDIUM, novo] src/c.rs:1: captura cortada\n" +
  "- F-05 [HIGH, baixa confiança, novo] src/d.rs:2: talvez\n\n" +
  "Resolvidos:\n- F-01 [HIGH] src/a.rs:1: falta validar\n\n" +
  "Critérios:\n✓ A: ok";

describe("estado dos apontamentos (spec 124 T01)", () => {
  it("lê novo, ainda aberto há N revisões e a seção de resolvidos", () => {
    const r = parseReviewMessage(TRACKED);
    expect(r?.findings.map((f) => [f.id, f.severity, f.state, f.recurrence])).toEqual([
      ["F-02", "HIGH", "open", 3],
      ["F-04", "MEDIUM", "new", undefined],
      ["F-05", "HIGH", "new", undefined],
    ]);
    expect(r?.findings[2]?.lowConfidence).toBe(true);
    expect(r?.findings[0]?.recommendation).toBe("adicionar");
    expect(r?.resolved).toEqual([
      { id: "F-01", severity: "HIGH", location: "src/a.rs:1", description: "falta validar", recommendation: "", state: "resolved" },
    ]);
    expect(r?.items).toHaveLength(1);
  });

  it("mensagem antiga, sem estados, não ganha selo nem resumo", () => {
    const r = parseReviewMessage(FULL);
    expect(r?.findings.every((f) => f.state === undefined)).toBe(true);
    expect(r?.resolved).toEqual([]);
    renderMessage(FULL);
    expect(screen.queryAllByTestId("review-finding-state")).toHaveLength(0);
    expect(screen.queryByTestId("review-state-summary")).toBeNull();
  });

  it("na Conversa: selos, resumo e resolvidos com o dicionário nos dois idiomas", () => {
    renderMessage(TRACKED);
    const states = screen.getAllByTestId("review-finding-state").map((e) => e.textContent);
    expect(states).toEqual(["ainda aberto há 3 revisões", "novo", "novo", "resolvido"]);
    expect(screen.getByTestId("review-state-summary").textContent).toBe("2 novos · 1 ainda aberto · 1 resolvido");
    const resolved = within(screen.getByRole("list", { name: "Apontamentos resolvidos" })).getAllByRole("listitem");
    expect(resolved).toHaveLength(1);
    expect(resolved[0]?.textContent).toContain("falta validar");
    cleanup();
    setLanguage("en", { persist: false });
    try {
      renderMessage(TRACKED);
      const en = screen.getAllByTestId("review-finding-state").map((e) => e.textContent);
      expect(en).toEqual(["still open for 3 reviews", "new", "new", "resolved"]);
      expect(screen.getByTestId("review-state-summary").textContent).toBe("2 new · 1 still open · 1 resolved");
      expect(screen.getByRole("list", { name: "Resolved findings" })).toBeTruthy();
      expect(screen.queryByText(/aberto há|Resolvidos/)).toBeNull();
    } finally {
      setLanguage("pt-BR", { persist: false });
    }
  });
});

/* Spec 124 T02: respostas do Desenvolvedor e decisões do Arquiteto (formato de `render_review_message_tracked`). */
const ARBITRATED =
  "Aprovado: tudo certo\n\nObservações (não bloqueiam):\n- F-01 [HIGH] src/gpu.rs:9: ainda sem WebGPU\n\n" +
  "Respostas do Desenvolvedor:\n- F-01 [contested, environment] o Docker não tem GPU\n- F-02 [fixed] src/a.rs:3\n- F-04 [talvez] x\n\n" +
  "Decisões do Arquiteto:\n- F-01 [manual] Falta WebGPU nativo: sem GPU no Docker\n\n" +
  "Critérios:\n✓ A: ok";

describe("respostas e decisões (spec 124 T02)", () => {
  it("lê as duas seções sem misturar com as observações", () => {
    const r = parseReviewMessage(ARBITRATED);
    expect(r?.observations.map((f) => f.id)).toEqual(["F-01"]);
    expect(r?.responses).toEqual([
      { id: "F-01", kind: "contested", reason: "environment", detail: "o Docker não tem GPU" },
      { id: "F-02", kind: "fixed", detail: "src/a.rs:3" },
      { id: "F-04", kind: "talvez", detail: "x" },
    ]);
    expect(r?.decisions).toEqual([{ id: "F-01", decision: "manual", detail: "Falta WebGPU nativo: sem GPU no Docker" }]);
    expect(r?.items).toHaveLength(1);
    expect(parseReviewMessage(FULL)?.responses).toEqual([]);
  });

  it("na Conversa: rótulos do dicionário nos dois idiomas; código desconhecido como veio", () => {
    renderMessage(ARBITRATED);
    const kinds = screen.getAllByTestId("review-response-kind").map((e) => e.textContent);
    expect(kinds).toEqual(["contestado", "corrigido", "talvez"]);
    expect(screen.getByTestId("review-contest-reason").textContent).toBe("inviável no ambiente");
    expect(screen.getByTestId("review-decision").textContent).toBe("verificação manual");
    expect(within(screen.getByRole("list", { name: "Respostas aos apontamentos" })).getAllByRole("listitem")).toHaveLength(3);
    expect(screen.getByRole("list", { name: "Decisões sobre apontamentos contestados" })).toBeTruthy();
    cleanup();
    setLanguage("en", { persist: false });
    try {
      renderMessage(ARBITRATED);
      expect(screen.getAllByTestId("review-response-kind").map((e) => e.textContent)).toEqual(["contested", "fixed", "talvez"]);
      expect(screen.getByTestId("review-contest-reason").textContent).toBe("not feasible in the environment");
      expect(screen.getByTestId("review-decision").textContent).toBe("manual check");
      expect(screen.getByText("Developer responses:")).toBeTruthy();
      expect(screen.getByText("Architect decisions:")).toBeTruthy();
      expect(screen.queryByText(/Respostas do Desenvolvedor|Decisões do Arquiteto/)).toBeNull();
    } finally {
      setLanguage("pt-BR", { persist: false });
    }
  });
});
