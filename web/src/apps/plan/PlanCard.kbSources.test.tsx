/*
 * Fontes da base no Plano (spec 136 T04): o quadro agrupado por documento, Regra e Consulta, a
 * fonte removida, a linha "não usa a base", a base vazia, o toque que abre o Conhecimento na seção
 * com o destaque, os critérios das tarefas e os globais com as citações como link, e a releitura
 * pelo `kb_updated`.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { KbDoc, KbPlanSource } from "../../api/kb";
import type { ApiEvent, SpecView } from "../../api/types";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { setLanguage } from "../../i18n";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";
import { applyKbEvent, resetKbStore } from "../knowledge/kbStore";
import { resetKbExcerpts } from "../knowledge/kbCiteStore";

const API = "padroes-api-3f9a1c2e";
const LGPD = "seguranca-lgpd-7b21e904";
const ADR = "adr-007-lembretes-91fe33c0";

function doc(id: string, title: string): KbDoc {
  return {
    id,
    title,
    kind: "markdown",
    source: "file",
    file_name: `${id}.md`,
    source_url: null,
    size: 10,
    added_at: "2026-10-28T12:00:00Z",
    fetched_at: null,
    status: "ready",
    pages: null,
    description: "",
    links_out: [],
  };
}

const DOCS = [doc(API, "Padrões de API"), doc(LGPD, "Guia de LGPD")];

const SOURCES: KbPlanSource[] = [
  {
    citation: `kb:${API}#versionamento`,
    doc: API,
    doc_title: "Padrões de API",
    anchor: "versionamento",
    resolved_anchor: "versionamento",
    section_title: "Versionamento",
    binding: true,
    removed: false,
    uses: [
      { in: "spec", section: "Premissas", structural: true, binding: true },
      { in: "task", task: "T02", section: "Critérios de Aceitação", structural: true, binding: true },
      { in: "task", task: "T02", section: "Critérios de Aceitação", structural: true, binding: true },
    ],
  },
  {
    citation: `kb:${LGPD}`,
    doc: LGPD,
    doc_title: "Guia de LGPD",
    anchor: null,
    resolved_anchor: null,
    section_title: null,
    binding: false,
    removed: false,
    uses: [{ in: "spec", section: "Abordagem Técnica", structural: true, binding: false }],
  },
  {
    citation: `kb:${API}#paginacao`,
    doc: API,
    doc_title: "Padrões de API",
    anchor: "paginacao",
    resolved_anchor: "paginacao",
    section_title: "Paginação",
    binding: false,
    removed: false,
    uses: [{ in: "spec", section: "Decisões locais", structural: false, binding: false }],
  },
  {
    citation: `kb:${ADR}#fila`,
    doc: ADR,
    doc_title: null,
    anchor: "fila",
    resolved_anchor: null,
    section_title: null,
    binding: true,
    removed: true,
    uses: [{ in: "task", task: "T03", section: "Critérios de Aceitação", structural: true, binding: true }],
  },
];

const NOTE = {
  content: "# Padrões de API\n\nIntro.\n\n## Versionamento\n\nPrefixo /v2.\n\n## Erros\n\nproblem+json.\n",
  sections: [
    { anchor: "padroes-de-api", title: "Padrões de API", level: 1, line: 0 },
    { anchor: "versionamento", title: "Versionamento", level: 2, line: 4 },
    { anchor: "erros", title: "Erros", level: 2, line: 8 },
  ],
};

function spec(): SpecView {
  return {
    ...makeSpec("012-agenda", "Agenda", "AWAITING_APPROVAL", [
      makeTask("T01", "Modelo", "TODO"),
      { ...makeTask("T02", "Rotas", "TODO", { depends_on: ["T01"] }), criteria: [`POST no prefixo de kb:${API}#versionamento.`, "DELETE cancela."] },
      { ...makeTask("T03", "Fila", "TODO"), criteria: [`Reenvio pela fila de kb:${ADR}#fila.`] },
    ]),
    criteria: [`Erros em problem+json, como em kb:${API}#erros.`],
    plan_review: { status: "ok", issues: [] },
  };
}

const PLAN_ONLY = { layouts: { planning: { current: { windows: { plan: { x: 0, y: 0, w: 900, h: 800, z: 1, mode: "normal", open: true } } } } } };

async function mount({ docs = DOCS, sources = SOURCES, plan = spec() }: { docs?: KbDoc[]; sources?: KbPlanSource[]; plan?: SpecView } = {}) {
  const view = await renderShell({
    url: "/?space=p1",
    projects: [makeProject("p1", "Agenda")],
    states: { p1: makeProjectState({ id: "p1", name: "Agenda", specs: [plan] }) },
    ui: { p1: { rev: 1, data: PLAN_ONLY } },
  });
  const model = { docs, sources };
  const { server } = view;
  server.reply("GET", /\/api\/projects\/p1\/kb$/, () => ({ status: 200, body: { docs: model.docs, has_overview: false } }));
  server.reply("GET", /\/kb-sources$/, () => ({ status: 200, body: { sources: model.sources } }));
  server.reply("GET", /\/kb\/docs\//, (_b, url) => {
    const id = decodeURIComponent(url.split("/").pop()?.split("?")[0] ?? "");
    const found = model.docs.find((d) => d.id === id);
    return found === undefined ? { status: 404, body: { error: "x", code: "kb_doc_not_found" } } : { status: 200, body: { doc: found, ...NOTE } };
  });
  // O plano montou antes das rotas: a geração nova da base relê a lista e as fontes.
  await act(async () => {
    applyKbEvent("p1");
  });
  await settle();
  return { ...view, model };
}

function plan(): HTMLElement {
  return screen.getByRole("region", { name: "Plano" });
}

function panel(): HTMLElement {
  const el = plan().querySelector<HTMLElement>("[data-kb-sources]");
  if (el === null) throw new Error("quadro ausente");
  return el;
}

beforeEach(() => {
  resetKbStore();
  resetKbExcerpts();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  setLanguage("pt-BR", { persist: false });
});

describe("Fontes da base: o quadro", () => {
  it("agrupa por documento, com a seção, os usos sem repetir e Regra ou Consulta", async () => {
    await mount();
    const box = panel();
    expect(box.querySelector("h3")?.textContent).toBe("Fontes da base");
    expect(box.querySelector(".nd-kb-sources-count")?.textContent).toBe("3 documentos · 4 citações");
    const groups = [...box.querySelectorAll<HTMLElement>("[data-kb-src-doc]")];
    expect(groups.map((g) => g.getAttribute("data-kb-src-doc"))).toEqual([API, LGPD, ADR]);
    const api = groups[0] as HTMLElement;
    expect(api.querySelector(".nd-kb-src-doc-title")?.textContent).toBe("Padrões de API");
    const rows = [...api.querySelectorAll<HTMLElement>(".nd-kb-src-item")];
    expect(rows.map((r) => r.querySelector(".nd-kb-src-section")?.textContent)).toEqual(["Versionamento", "Paginação"]);
    expect(rows[0]?.querySelector(".nd-kb-src-where")?.textContent).toBe("Premissas, T02 · Critérios de Aceitação");
    expect(rows[0]?.querySelector("[data-kb-src-kind]")?.getAttribute("data-kb-src-kind")).toBe("binding");
    expect(rows[0]?.querySelector("[data-kb-src-kind]")?.textContent).toBe("Regra");
    // Título livre fica como está; o estrutural é traduzido.
    expect(rows[1]?.querySelector(".nd-kb-src-where")?.textContent).toBe("Decisões locais");
    expect(rows[1]?.querySelector("[data-kb-src-kind]")?.textContent).toBe("Consulta");
    const lgpd = groups[1] as HTMLElement;
    expect(lgpd.querySelector(".nd-kb-src-section")?.textContent).toBe("Documento inteiro");
    expect(box.querySelector(".nd-kb-sources-legend")?.textContent).toContain("transcrita no plano; obriga");
  });

  it("a fonte removida fica marcada, com o nome do id, e não abre", async () => {
    await mount();
    const removed = panel().querySelector<HTMLElement>(`[data-kb-src-doc="${ADR}"]`);
    if (removed === null) throw new Error("grupo removido ausente");
    expect(removed.getAttribute("data-removed")).toBe("true");
    expect(removed.querySelector(".nd-kb-src-doc-title")?.textContent).toBe("Documento removido");
    expect(removed.querySelector(".nd-kb-src-doc-id")?.textContent).toBe("adr-007-lembretes");
    expect(removed.textContent).toContain("Saiu da base");
    const item = removed.querySelector(".nd-kb-src-item");
    expect(item?.tagName).toBe("LI");
    expect(item?.getAttribute("data-removed")).toBe("true");
  });

  it("com a base e sem citação, uma linha discreta; com a base vazia, nada", async () => {
    await mount({ sources: [] });
    expect(plan().querySelector("[data-kb-sources]")).toBeNull();
    expect(plan().querySelector("[data-kb-sources-none]")?.textContent).toBe("O plano não usa a base de conhecimento (2 documentos).");
    cleanup();
    resetKbStore();
    await mount({ sources: [], docs: [] });
    expect(plan().querySelector("[data-kb-sources]")).toBeNull();
    expect(plan().querySelector("[data-kb-sources-none]")).toBeNull();
  });

  it("um documento: singular", async () => {
    await mount({ sources: [], docs: [doc(API, "Padrões de API")] });
    expect(plan().querySelector("[data-kb-sources-none]")?.textContent).toBe("O plano não usa a base de conhecimento (1 documento).");
  });

  it("o toque abre o Conhecimento na seção, que se destaca por um instante", async () => {
    await mount();
    const row = panel().querySelector<HTMLButtonElement>(`[data-kb-src="kb:${API}#versionamento"]`);
    if (row === null) throw new Error("linha ausente");
    expect(row.getAttribute("aria-label")).toBe('Abrir a seção "Versionamento" de "Padrões de API" no Conhecimento');
    await act(async () => {
      fireEvent.click(row);
    });
    await settle();
    const knowledge = screen.getByRole("region", { name: "Conhecimento" });
    const cited = knowledge.querySelector<HTMLElement>('[data-kb-cited="versionamento"]');
    expect(cited?.textContent).toContain("Prefixo /v2.");
    expect(cited?.getAttribute("data-flash")).toBe("true");
  });

  it("kb_updated relê as fontes", async () => {
    const { server, stream, model } = await mount();
    const before = server.count("GET", /\/kb-sources$/);
    model.sources = [];
    await act(async () => {
      const event = { event_type: "kb_updated", project_id: "p1", payload: { id: API, status: "ready", status_reason: null, doc: { id: API } } } as unknown as ApiEvent;
      stream().emit("kb_updated", event);
    });
    await settle();
    expect(server.count("GET", /\/kb-sources$/)).toBeGreaterThan(before);
    expect(plan().querySelector("[data-kb-sources]")).toBeNull();
    expect(plan().querySelector("[data-kb-sources-none]")).not.toBeNull();
  });

  it("em inglês: título, contagem, marcas e lugares traduzidos", async () => {
    setLanguage("en", { persist: false });
    await mount();
    const box = screen.getByRole("region", { name: "Plan" }).querySelector<HTMLElement>("[data-kb-sources]");
    expect(box?.querySelector("h3")?.textContent).toBe("Knowledge sources");
    expect(box?.querySelector(".nd-kb-sources-count")?.textContent).toBe("3 documents · 4 citations");
    expect(box?.querySelector(".nd-kb-src-where")?.textContent).toBe("Assumptions, T02 · Acceptance Criteria");
    expect(box?.querySelector("[data-kb-src-kind]")?.textContent).toBe("Rule");
  });
});

describe("Critérios com as citações como link", () => {
  it("a tarefa com critérios abre a lista com os links; a sem critérios não abre", async () => {
    await mount();
    const toggle = within(plan()).getByRole("button", { name: "T02 Rotas" });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(toggle.querySelector("[data-task-cites]")?.textContent).toBe("1");
    await act(async () => {
      fireEvent.click(toggle);
    });
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    const list = within(plan()).getByRole("list", { name: "Critérios de T02" });
    expect([...list.querySelectorAll("[data-kb-cite]")].map((c) => c.getAttribute("data-kb-cite"))).toEqual([`kb:${API}#versionamento`]);
    expect(list.textContent).toContain("DELETE cancela.");
    expect(within(plan()).queryByRole("button", { name: "T01 Modelo" })).toBeNull();
  });

  it("a citação mostra o título da seção (spec 136 T06), com um pedido só por documento e relido no kb_updated", async () => {
    const { server } = await mount();
    await act(async () => {
      fireEvent.click(within(plan()).getByRole("button", { name: "T02 Rotas" }));
    });
    const details = plan().querySelector<HTMLDetailsElement>("details[data-plan-more]");
    if (details !== null) details.open = true;
    await settle();
    const versioning = plan().querySelector<HTMLElement>(`[data-kb-cite="kb:${API}#versionamento"]`);
    expect(versioning?.querySelector(".nd-kb-cite-section")?.textContent).toBe("#Versionamento");
    expect(versioning?.getAttribute("aria-label")).toContain("Versionamento");
    expect(plan().querySelector(`[data-kb-cite="kb:${API}#erros"] .nd-kb-cite-section`)?.textContent).toBe("#Erros");
    const outlineReads = (): number => server.calls.filter((c) => c.method === "GET" && c.url.endsWith(`/kb/docs/${API}`)).length;
    expect(outlineReads()).toBe(1);
    await act(async () => {
      applyKbEvent("p1");
    });
    await settle();
    expect(outlineReads()).toBe(2);
  });

  it("a citação de um documento que saiu da base fica marcada", async () => {
    await mount();
    await act(async () => {
      fireEvent.click(within(plan()).getByRole("button", { name: "T03 Fila" }));
    });
    const cite = plan().querySelector<HTMLElement>(`[data-kb-cite="kb:${ADR}#fila"]`);
    expect(cite?.getAttribute("data-removed")).toBe("true");
  });

  it("os critérios globais ficam no detalhe, com a citação como link", async () => {
    await mount();
    const details = plan().querySelector<HTMLDetailsElement>("details[data-plan-more]");
    if (details === null) throw new Error("detalhe ausente");
    expect(details.querySelector("summary")?.textContent).toContain("Critérios globais");
    details.open = true;
    const section = details.querySelector<HTMLElement>("[data-plan-criteria]");
    expect(section?.querySelector("h3")?.textContent).toBe("Critérios de Aceitação Globais");
    expect(section?.querySelector("[data-kb-cite]")?.getAttribute("data-kb-cite")).toBe(`kb:${API}#erros`);
  });
});
