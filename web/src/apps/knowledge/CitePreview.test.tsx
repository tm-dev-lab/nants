/*
 * Prévia da citação `kb:` (spec 136 T04, critério 4): no desktop, o cartão abre depois da espera ao
 * passar o mouse ou focar, com o documento, a seção, o trecho e "Abrir na base"; Esc fecha e devolve
 * o foco; o trecho fica guardado por sessão e o `kb_updated` o descarta. No celular, o toque abre a
 * folha inferior.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { KbDoc } from "../../api/kb";
import type { SpecView } from "../../api/types";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { setLanguage } from "../../i18n";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";
import { resetKbExcerpts } from "./kbCiteStore";
import { applyKbEvent, resetKbStore } from "./kbStore";

const API = "padroes-api-3f9a1c2e";
const LGPD = "seguranca-lgpd-7b21e904";

function doc(id: string, title: string, kind: KbDoc["kind"] = "markdown"): KbDoc {
  return {
    id,
    title,
    kind,
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

const SECTION = {
  anchor: "dados-pessoais",
  title: "Dados pessoais",
  trail: ["Guia de LGPD", "Registro de eventos"],
  page: 17,
  text: "Nada de CPF em log.",
  truncated: true,
  max_bytes: 8192,
};

function spec(): SpecView {
  return {
    ...makeSpec("012-agenda", "Agenda", "AWAITING_APPROVAL", [
      { ...makeTask("T01", "Lembrete", "TODO"), criteria: [`O evento leva só o id, conforme kb:${LGPD}#dados-pessoais.`] },
    ]),
    plan_review: { status: "ok", issues: [] },
  };
}

const PLAN = { x: 0, y: 0, w: 900, h: 800, z: 1, mode: "normal", open: true };

async function mount() {
  const view = await renderShell({
    url: "/?space=p1",
    projects: [makeProject("p1", "Agenda")],
    states: { p1: makeProjectState({ id: "p1", name: "Agenda", specs: [spec()] }) },
    ui: {
      p1: {
        rev: 1,
        data: { layouts: { planning: { current: { windows: { plan: PLAN } } } }, mobile: { view: "plan", category: "planning", selection: {} } },
      },
    },
  });
  const { server } = view;
  server.reply("GET", /\/api\/projects\/p1\/kb$/, () => ({ status: 200, body: { docs: [doc(API, "Padrões de API"), doc(LGPD, "Guia de LGPD", "pdf")], has_overview: false } }));
  server.reply("GET", /\/kb-sources$/, () => ({ status: 200, body: { sources: [] } }));
  server.reply("GET", /\/kb\/docs\/[^/?]+\?section=/, () => ({ status: 200, body: { doc: doc(LGPD, "Guia de LGPD", "pdf"), section: SECTION } }));
  await act(async () => {
    applyKbEvent("p1");
  });
  await settle();
  const toggle = screen.getByRole("button", { name: "T01 Lembrete" });
  await act(async () => {
    fireEvent.click(toggle);
  });
  return view;
}

function cite(): HTMLElement {
  const el = document.querySelector<HTMLElement>(`[data-kb-cite="kb:${LGPD}#dados-pessoais"]`);
  if (el === null) throw new Error("citação ausente");
  return el;
}

const wait = (ms: number) =>
  act(async () => {
    await new Promise((resolve) => setTimeout(resolve, ms));
  });

beforeEach(() => {
  resetKbStore();
  resetKbExcerpts();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  setLanguage("pt-BR", { persist: false });
});

describe("Prévia da citação no desktop", () => {
  it("abre depois da espera, com o documento, a seção, o trecho e Abrir na base", async () => {
    const { server } = await mount();
    fireEvent.mouseEnter(cite());
    await wait(100);
    expect(document.querySelector("[data-kb-card]")).toBeNull();
    await wait(250);
    await settle();
    const card = document.querySelector<HTMLElement>("[data-kb-card]");
    if (card === null) throw new Error("cartão ausente");
    expect(card.getAttribute("aria-label")).toBe('Prévia de "Guia de LGPD"');
    expect(card.querySelector(".nd-kb-card-doc-title")?.textContent).toBe("Guia de LGPD");
    expect(card.querySelector(".nd-kb-card-page")?.textContent).toBe("p. 17");
    expect(card.querySelector(".nd-kb-card-trail")?.textContent).toBe("Registro de eventos");
    expect(card.querySelector(".nd-kb-card-title")?.textContent).toBe("Dados pessoais");
    expect(card.querySelector(".nd-kb-card-text")?.textContent).toContain("Nada de CPF em log.");
    expect(server.calls.some((c) => c.url.endsWith(`/kb/docs/${LGPD}?section=dados-pessoais`))).toBe(true);
    await act(async () => {
      fireEvent.click(within(card).getByRole("button", { name: "Abrir na base" }));
    });
    await settle();
    expect(document.querySelector("[data-kb-card]")).toBeNull();
    expect(screen.getByRole("region", { name: "Conhecimento" })).toBeTruthy();
  });

  it("sair com o mouse fecha; o foco pelo teclado abre e o Esc fecha e devolve o foco", async () => {
    await mount();
    fireEvent.mouseEnter(cite());
    await wait(300);
    expect(document.querySelector("[data-kb-card]")).not.toBeNull();
    fireEvent.mouseLeave(cite());
    await wait(250);
    expect(document.querySelector("[data-kb-card]")).toBeNull();
    await act(async () => {
      cite().focus();
    });
    await wait(300);
    expect(document.querySelector("[data-kb-card]")).not.toBeNull();
    expect(cite().getAttribute("aria-expanded")).toBe("true");
    fireEvent.keyDown(cite(), { key: "Escape" });
    expect(document.querySelector("[data-kb-card]")).toBeNull();
    expect(document.activeElement).toBe(cite());
  });

  it("guarda o trecho por sessão; o kb_updated o descarta", async () => {
    const { server } = await mount();
    const reads = () => server.count("GET", /\?section=/);
    fireEvent.mouseEnter(cite());
    await wait(300);
    await settle();
    fireEvent.mouseLeave(cite());
    await wait(250);
    fireEvent.mouseEnter(cite());
    await wait(300);
    await settle();
    // Do cache, o trecho aparece no primeiro quadro, sem o carregamento.
    expect(document.querySelector("[data-kb-card] .nd-kb-card-skeleton")).toBeNull();
    expect(reads()).toBe(1);
    fireEvent.mouseLeave(cite());
    await wait(250);
    await act(async () => {
      applyKbEvent("p1");
    });
    await settle();
    fireEvent.mouseEnter(cite());
    await wait(300);
    await settle();
    expect(reads()).toBe(2);
  });
});

describe("Prévia da citação no celular", () => {
  it("o toque abre a folha inferior; o Esc fecha", async () => {
    vi.stubGlobal("matchMedia", (query: string) => ({
      matches: true,
      media: query,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
    }));
    await mount();
    await act(async () => {
      fireEvent.click(cite());
    });
    await settle();
    const sheet = document.querySelector<HTMLElement>("[data-kb-sheet]");
    if (sheet === null) throw new Error("folha ausente");
    expect(sheet.getAttribute("aria-modal")).toBe("true");
    expect(sheet.querySelector(".nd-kb-card-title")?.textContent).toBe("Dados pessoais");
    expect(sheet.textContent).toContain("Trecho cortado. A seção inteira está na base.");
    expect(within(sheet).getByRole("button", { name: "Abrir na base" })).toBeTruthy();
    // Sem cartão do desktop no celular.
    expect(document.querySelector("[data-kb-card]")).toBeNull();
    // Tab preso na folha: o foco começa no primeiro controle; do último volta ao primeiro e vice-versa.
    const controls = [...sheet.querySelectorAll<HTMLElement>("button")];
    const first = controls[0];
    const last = controls[controls.length - 1];
    if (first === undefined || last === undefined || first === last) throw new Error("controles da folha ausentes");
    expect(document.activeElement).toBe(first);
    last.focus();
    fireEvent.keyDown(last, { key: "Tab" });
    expect(document.activeElement).toBe(first);
    fireEvent.keyDown(first, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(last);
    fireEvent.keyDown(last, { key: "Escape" });
    expect(document.querySelector("[data-kb-sheet]")).toBeNull();
    expect(document.activeElement).toBe(cite());
  });

  it("fechar pelo botão ou pelo toque fora devolve o foco à citação", async () => {
    vi.stubGlobal("matchMedia", (query: string) => ({
      matches: true,
      media: query,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
    }));
    await mount();
    await act(async () => {
      fireEvent.click(cite());
    });
    await settle();
    const sheet = document.querySelector<HTMLElement>("[data-kb-sheet]");
    if (sheet === null) throw new Error("folha ausente");
    fireEvent.click(within(sheet).getByRole("button", { name: "Fechar" }));
    expect(document.querySelector("[data-kb-sheet]")).toBeNull();
    expect(document.activeElement).toBe(cite());
    await act(async () => {
      fireEvent.click(cite());
    });
    await settle();
    const backdrop = document.querySelector<HTMLElement>("[data-kb-sheet-backdrop]");
    if (backdrop === null) throw new Error("fundo ausente");
    (document.activeElement as HTMLElement | null)?.blur();
    fireEvent.mouseDown(backdrop);
    expect(document.querySelector("[data-kb-sheet]")).toBeNull();
    expect(document.activeElement).toBe(cite());
  });
});
