/*
 * Navegação na janela Conhecimento (spec 136 T07): a busca rápida (espera, mínimo de letras,
 * cancelamento, descarte fora de ordem e teclado), o trecho só como texto (C-10), o `snippetParts`
 * com acento e emoji, o sumário e as ligações do documento.
 */
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { KbDoc, KbNote, KbSearchHit } from "../../api/kb";
import { KbOutline } from "./KbOutline";
import { KbQuickSearch, Snippet } from "./KbQuickSearch";
import { contextParts, KbRelations } from "./KbRelations";
import { resetKbExcerpts } from "./kbCiteStore";
import { KB_SEARCH_DEBOUNCE_MS, snippetParts } from "./kbSearchModel";

const api = vi.hoisted(() => ({
  searchKb: vi.fn(),
  getKbDoc: vi.fn(),
}));

vi.mock("../../api/kb", async (importOriginal) => {
  const real = await importOriginal<typeof import("../../api/kb")>();
  return { ...real, searchKb: api.searchKb, getKbDoc: api.getKbDoc };
});

const API = "padroes-api-3f9a1c2e";
const LGPD = "seguranca-lgpd-7b21e904";
const GLOSS = "glossario-4d3c2b1a";

function doc(id: string, title: string, extra: Partial<KbDoc> = {}): KbDoc {
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
    ...extra,
  };
}

const DOCS = [
  doc(API, "Padrões de API REST"),
  doc(LGPD, "Guia de segurança e LGPD", { kind: "pdf" }),
  doc(GLOSS, "Glossário", { indicators: [{ code: "no_headings" }] }),
];

function hit(partial: Partial<KbSearchHit> = {}): KbSearchHit {
  return {
    doc: API,
    doc_title: "Padrões de API REST",
    anchor: "seguranca",
    title: "Segurança",
    trail: "Padrões de API REST > Segurança",
    page: null,
    snippet: "Toda rota exige o token. Veja a segurança.",
    highlights: [[33, 42]],
    ...partial,
  };
}

/** Pedido de busca controlado pelo teste (resolve quando ele mandar). */
function deferred() {
  let resolve: (value: unknown) => void = () => {};
  const promise = new Promise((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

const ok = (results: KbSearchHit[]) => ({ ok: true, data: { results } });

beforeEach(() => {
  vi.useFakeTimers();
  resetKbExcerpts();
  api.searchKb.mockReset();
  api.getKbDoc.mockReset();
  api.getKbDoc.mockResolvedValue({
    ok: true,
    data: {
      doc: DOCS[0],
      content: "",
      sections: [
        { anchor: "erros", title: "Erros", level: 2, line: 0 },
        { anchor: "seguranca", title: "Segurança", level: 2, line: 1 },
      ],
    },
  });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

function mountSearch(onOpen = vi.fn()) {
  render(<KbQuickSearch projectId="p1" docs={DOCS} variant="popover" onOpen={onOpen} />);
  const input = screen.getByRole("combobox", { name: "Buscar na base de conhecimento" });
  return { input, onOpen };
}

async function typeIn(input: HTMLElement, value: string) {
  await act(async () => {
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value } });
  });
}

async function wait(ms: number) {
  await act(async () => {
    vi.advanceTimersByTime(ms);
  });
  await act(async () => {});
}

describe("snippetParts", () => {
  it("posições de caractere Unicode: acento e emoji não deslocam o destaque", () => {
    const text = "Café 🚀 seguro e ação";
    const chars = Array.from(text);
    const at = chars.indexOf("s");
    const parts = snippetParts(text, [[at, at + 6], [chars.length - 4, chars.length]]);
    expect(parts.filter((p) => p.hit).map((p) => p.text)).toEqual(["seguro", "ação"]);
    expect(parts.map((p) => p.text).join("")).toBe(text);
  });

  it("fora de ordem, sobreposto ou fora do trecho: ajustado ou descartado, o texto sai inteiro", () => {
    const parts = snippetParts("abcdef", [[4, 99], [0, 2], [1, 3], [5, 5], [-3, -1]]);
    expect(parts).toEqual([
      { text: "ab", hit: true },
      { text: "c", hit: true },
      { text: "d", hit: false },
      { text: "ef", hit: true },
    ]);
  });
});

describe("trecho só como texto (C-10)", () => {
  it("um trecho com <img onerror> vira texto; o destaque é <mark>", () => {
    const evil = '<img src=x onerror="alert(1)"> segurança';
    const { container } = render(<Snippet text={evil} highlights={[[Array.from(evil).length - 9, Array.from(evil).length]]} />);
    expect(container.querySelector("img")).toBeNull();
    expect(container.textContent).toBe(evil);
    expect(container.querySelector("mark")?.textContent).toBe("segurança");
  });

  it("a linha de quem cita também: link e código viram texto, sem HTML", () => {
    expect(contextParts('- Veja o [guia](padroes-api.md "t") e `type` <script>x</script>')).toEqual([
      { text: "Veja o ", kind: "text" },
      { text: "guia", kind: "link" },
      { text: " e ", kind: "text" },
      { text: "type", kind: "code" },
      { text: " <script>x</script>", kind: "text" },
    ]);
  });
});

describe("busca rápida", () => {
  it("com 1 letra só filtra os títulos; o texto espera a pausa e o mínimo de 2 letras", async () => {
    api.searchKb.mockResolvedValue(ok([hit()]));
    const { input } = mountSearch();
    await typeIn(input, "g");
    await wait(1000);
    expect(api.searchKb).not.toHaveBeenCalled();
    expect(screen.getByText("Com 2 letras, a busca entra também no texto.")).toBeTruthy();
    expect(screen.getByRole("option", { name: /Guia de segurança/ })).toBeTruthy();

    await typeIn(input, "se");
    await wait(KB_SEARCH_DEBOUNCE_MS - 1);
    expect(api.searchKb).not.toHaveBeenCalled();
    await typeIn(input, "seg");
    await wait(KB_SEARCH_DEBOUNCE_MS - 1);
    expect(api.searchKb).not.toHaveBeenCalled();
    await wait(1);
    expect(api.searchKb).toHaveBeenCalledTimes(1);
    expect(api.searchKb.mock.calls[0]?.slice(0, 2)).toEqual(["p1", "seg"]);
    expect(screen.getByText("1 trecho")).toBeTruthy();
  });

  it("cada busca nova cancela a anterior, e a resposta velha que chega depois é descartada", async () => {
    const first = deferred();
    const second = deferred();
    api.searchKb.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const { input } = mountSearch();
    await typeIn(input, "token");
    await wait(KB_SEARCH_DEBOUNCE_MS);
    const firstSignal = api.searchKb.mock.calls[0]?.[3] as AbortSignal;
    expect(firstSignal.aborted).toBe(false);

    await typeIn(input, "tokens");
    expect(firstSignal.aborted).toBe(true);
    await wait(KB_SEARCH_DEBOUNCE_MS);
    expect(api.searchKb).toHaveBeenCalledTimes(2);

    await act(async () => {
      second.resolve(ok([hit({ anchor: "nova", title: "Resposta nova" })]));
    });
    await act(async () => {
      first.resolve(ok([hit({ anchor: "velha", title: "Resposta velha" })]));
    });
    expect(screen.getByText("Resposta nova")).toBeTruthy();
    expect(screen.queryByText("Resposta velha")).toBeNull();
  });

  it("teclado: setas passam pelos dois grupos, Enter abre o trecho na seção", async () => {
    api.searchKb.mockResolvedValue(ok([hit()]));
    const { input, onOpen } = mountSearch();
    await typeIn(input, "segurança");
    await wait(KB_SEARCH_DEBOUNCE_MS);
    const options = screen.getAllByRole("option");
    expect(options).toHaveLength(2);
    expect(input.getAttribute("aria-activedescendant")).toBe(options[0]?.id);
    fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(input.getAttribute("aria-activedescendant")).toBe(options[1]?.id);
    fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(input.getAttribute("aria-activedescendant")).toBe(options[0]?.id);
    fireEvent.keyDown(input, { key: "ArrowUp" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onOpen).toHaveBeenCalledWith(API, "seguranca");
    expect(input.getAttribute("aria-expanded")).toBe("false");
  });

  it("teclado: Enter no título abre o documento; Tab entra nas seções; Esc volta, fecha e limpa", async () => {
    api.searchKb.mockResolvedValue(ok([]));
    const { input, onOpen } = mountSearch();
    await typeIn(input, "padr");
    await wait(KB_SEARCH_DEBOUNCE_MS);
    fireEvent.keyDown(input, { key: "Tab" });
    await wait(0);
    expect(screen.getByTitle("Seções de Padrões de API REST")).toBeTruthy();
    expect(screen.getByRole("option", { name: /Segurança/ })).toBeTruthy();
    fireEvent.keyDown(input, { key: "Escape" });
    expect(screen.queryByTitle("Seções de Padrões de API REST")).toBeNull();
    expect(input.getAttribute("aria-expanded")).toBe("true");
    fireEvent.keyDown(input, { key: "Escape" });
    expect(input.getAttribute("aria-expanded")).toBe("false");
    expect((input as HTMLInputElement).value).toBe("padr");
    fireEvent.keyDown(input, { key: "Escape" });
    expect((input as HTMLInputElement).value).toBe("");

    await typeIn(input, "padr");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onOpen).toHaveBeenCalledWith(API, null);
  });

  it("nada nos títulos nem no texto: o estado vazio explica onde a busca olha", async () => {
    api.searchKb.mockResolvedValue(ok([]));
    const { input } = mountSearch();
    await typeIn(input, "zzz");
    await wait(KB_SEARCH_DEBOUNCE_MS);
    expect(screen.getByText('Nada na base com "zzz".')).toBeTruthy();
  });
});

describe("sumário", () => {
  const sections = [
    { anchor: "erros", title: "Erros", level: 2 },
    { anchor: "validacao", title: "Validação", level: 3 },
    { anchor: "paginacao", title: "Paginação", level: 2 },
  ];

  it("ao lado: a seção em leitura marcada, e o toque pula para ela", () => {
    const onJump = vi.fn();
    render(<KbOutline sections={sections} current="validacao" onJump={onJump} variant="side" />);
    const nav = screen.getByRole("navigation", { name: "Sumário" });
    expect(within(nav).getByRole("button", { name: "Validação" }).getAttribute("aria-current")).toBe("location");
    expect(within(nav).getByRole("button", { name: "Erros" }).getAttribute("aria-current")).toBeNull();
    expect(within(nav).getByRole("button", { name: "Validação" }).getAttribute("data-depth")).toBe("1");
    fireEvent.click(within(nav).getByRole("button", { name: "Paginação" }));
    expect(onJump).toHaveBeenCalledWith("paginacao");
  });

  it("recolhido: mostra a seção em leitura; abre, pula e fecha", () => {
    const onJump = vi.fn();
    render(<KbOutline sections={sections} current="paginacao" onJump={onJump} variant="collapsed" />);
    const toggle = screen.getByRole("button", { name: /Sumário/ });
    expect(toggle.textContent).toContain("Paginação");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(toggle);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    fireEvent.click(screen.getByRole("button", { name: "Erros" }));
    expect(onJump).toHaveBeenCalledWith("erros");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
  });
});

describe("ligações do documento", () => {
  const base: KbNote = { doc: DOCS[0] as KbDoc, content: "", sections: [], links_to: { docs: [], outside: [] }, cited_by: [], used_in_plans: [] };
  const sections = [
    { anchor: "versionamento", title: "Versionamento", level: 2 },
    { anchor: "erros", title: "Erros", level: 2 },
  ];

  it("sem nenhuma ligação, nada aparece", () => {
    const { container } = render(<KbRelations note={base} docs={DOCS} sections={sections} onOpenDoc={vi.fn()} onJump={vi.fn()} />);
    expect(container.childNodes).toHaveLength(0);
  });

  it("cada lista vazia some; as cheias mostram Regra e Consulta, fora da base e o link externo em aba nova", () => {
    const onOpenDoc = vi.fn();
    const onJump = vi.fn();
    const note: KbNote = {
      ...base,
      used_in_plans: [
        {
          spec: "012-agenda",
          title: "Agenda",
          frozen: true,
          uses: [
            { in: "spec", section: "Premissas", structural: true, binding: true, anchor: "versionamento" },
            { in: "task", task: "T02", section: "Abordagem Técnica", structural: true, binding: false, anchor: "erros" },
          ],
        },
      ],
      links_to: {
        docs: [{ id: LGPD, title: "Guia de segurança e LGPD", status: "ready" }],
        outside: [
          { kind: "url", target: "https://www.rfc-editor.org/rfc/rfc9457" },
          { kind: "file", target: "glossario-de-erros.md" },
        ],
      },
    };
    render(<KbRelations note={note} docs={DOCS} sections={sections} onOpenDoc={onOpenDoc} onJump={onJump} />);
    expect(screen.queryByRole("region", { name: /Citado por/ })).toBeNull();
    const plans = screen.getByRole("region", { name: /Usado nos planos/ });
    expect(within(plans).getByText("Aprovado")).toBeTruthy();
    const rule = within(plans).getByRole("button", { name: 'Ir para "Versionamento"' });
    expect(within(rule).getByText("Regra")).toBeTruthy();
    expect(within(rule).getByText("Premissas")).toBeTruthy();
    const ref = within(plans).getByRole("button", { name: 'Ir para "Erros"' });
    expect(within(ref).getByText("Consulta")).toBeTruthy();
    expect(within(ref).getByText("T02 · Abordagem Técnica")).toBeTruthy();
    fireEvent.click(rule);
    expect(onJump).toHaveBeenCalledWith("versionamento");

    const links = screen.getByRole("region", { name: /Links para/ });
    fireEvent.click(within(links).getByRole("button", { name: 'Abrir "Guia de segurança e LGPD"' }));
    expect(onOpenDoc).toHaveBeenCalledWith(LGPD);
    const external = within(links).getByRole("link");
    expect(external.getAttribute("href")).toBe("https://www.rfc-editor.org/rfc/rfc9457");
    expect(external.getAttribute("target")).toBe("_blank");
    expect(external.getAttribute("rel")?.split(" ").sort()).toEqual(["noopener", "noreferrer"]);
    expect(within(links).getByText("Fora da base")).toBeTruthy();
    expect(within(links).getByText("glossario-de-erros.md")).toBeTruthy();
  });

  it("alvo de fora que não é página http(s) fica como texto, sem link (F-01)", () => {
    const targets = ["javascript:alert(1)", "file:///etc/passwd", "data:text/html,<script>alert(1)</script>", "não é endereço"];
    const note: KbNote = { ...base, links_to: { docs: [], outside: targets.map((target) => ({ kind: "url" as const, target })) } };
    const { container } = render(<KbRelations note={note} docs={DOCS} sections={sections} onOpenDoc={vi.fn()} onJump={vi.fn()} />);
    const links = screen.getByRole("region", { name: /Links para/ });
    expect(within(links).queryByRole("link")).toBeNull();
    expect(container.querySelector("[href]")).toBeNull();
    for (const target of targets) expect(within(links).getByText(target)).toBeTruthy();
  });

  it("Citado por: a linha em volta do link, e o toque abre o documento que cita", () => {
    const onOpenDoc = vi.fn();
    const note: KbNote = { ...base, cited_by: [{ id: LGPD, title: "Guia de segurança e LGPD", status: "ready", context: "Siga os [padrões](padroes-api.md) na API." }] };
    render(<KbRelations note={note} docs={DOCS} sections={sections} onOpenDoc={onOpenDoc} onJump={vi.fn()} />);
    const cited = screen.getByRole("region", { name: /Citado por/ });
    expect(cited.textContent).toContain("Siga os padrões na API.");
    expect(within(cited).getByText("padrões").className).toContain("nd-kb-rel-context-link");
    expect(screen.queryByRole("region", { name: /Links para/ })).toBeNull();
    fireEvent.click(within(cited).getByRole("button"));
    expect(onOpenDoc).toHaveBeenCalledWith(LGPD);
  });
});
