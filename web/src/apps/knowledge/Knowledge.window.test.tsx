/*
 * Janela Conhecimento montada pelo shell (spec 134 T06): lista e subtítulo vivo, estados,
 * indicadores, estado vazio, envio de arquivos, link, edição da descrição, remoção com confirmação,
 * Visão geral (com a ajuda de que é ali que se declaram os padrões da empresa), o SSE `kb_updated`,
 * a citação `kb:` do Plano e os erros por código. Sem classificação manual (spec 136 T02).
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { KbDoc } from "../../api/kb";
import type { ApiEvent, SpecView } from "../../api/types";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { windowSubtitleId } from "../../desktop/windowMeta";
import { setLanguage } from "../../i18n";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";
import { reloadKb, resetKbStore } from "./kbStore";

const P1 = makeProject("p1", "Loja");
const API = "padroes-api-3f9a1c2e";
const LINK = "arquitetura-0d6b72ae";
const GATEWAY = "manual-gateway-c48e1f93";
const SCAN = "contrato-9e14c5b8";
const COPY = "copia-api-6c3e8d51";

function doc(partial: Partial<KbDoc> & Pick<KbDoc, "id" | "title" | "kind">): KbDoc {
  return {
    source: partial.kind === "link" ? "url" : "file",
    file_name: null,
    source_url: null,
    size: 0,
    added_at: "2026-10-28T12:00:00Z",
    fetched_at: null,
    status: "ready",
    status_reason: null,
    pages: null,
    description: "",
    links_out: [],
    indicators: [],
    ...partial,
  };
}

function docs(): KbDoc[] {
  return [
    doc({ id: API, title: "Padrões de API", kind: "markdown", file_name: "padroes-api.md", size: 2048, description: "Rotas e erros" }),
    doc({
      id: LINK,
      title: "Arquitetura hexagonal",
      kind: "link",
      source_url: "https://eng.exemplo.com/hexagonal",
      indicators: [{ code: "broken_link", params: { target: "banco.md" } }],
    }),
    doc({ id: GATEWAY, title: "Manual do gateway", kind: "pdf", file_name: "gateway.pdf", status: "extracting" }),
    doc({ id: SCAN, title: "Contrato", kind: "pdf", file_name: "contrato.pdf", status: "no_text", status_reason: "kb_pdf_no_text" }),
    doc({ id: COPY, title: "Cópia da API", kind: "link", source_url: "https://wiki.exemplo.com/api", indicators: [{ code: "duplicate", params: { doc: API } }] }),
  ];
}

const NOTE = {
  content: "# Padrões de API\n\nIntro.\n\n## Versionamento\n\nPrefixo /v2.\n\n## Erros\n\nproblem+json.\n",
  sections: [
    { anchor: "padroes-de-api", title: "Padrões de API", level: 1, line: 0 },
    { anchor: "versionamento", title: "Versionamento", level: 2, line: 4 },
    { anchor: "erros", title: "Erros", level: 2, line: 8 },
  ],
};

const OPEN_LAYOUT = {
  layouts: { planning: { current: { windows: { knowledge: { x: 0, y: 0, w: 1000, h: 700, z: 1, mode: "normal", open: true } } } } },
};

interface MountOptions {
  list?: KbDoc[];
  spec?: SpecView;
  layout?: Record<string, unknown>;
}

async function mount({ list = docs(), spec, layout = OPEN_LAYOUT }: MountOptions = {}) {
  const view = await renderShell({
    url: "/?space=p1",
    projects: [P1],
    states: { p1: makeProjectState({ id: "p1", name: "Loja", specs: spec === undefined ? [] : [spec] }) },
    ui: { p1: { rev: 1, data: layout } },
  });
  const model = { docs: list, overview: "Resumo do projeto." };
  const { server } = view;
  server.reply("GET", /\/api\/projects\/p1\/kb$/, () => ({ status: 200, body: { docs: model.docs, has_overview: true } }));
  server.reply("GET", /\/kb\/docs\//, (_b, url) => {
    const id = decodeURIComponent(url.split("/").pop() ?? "");
    const found = model.docs.find((d) => d.id === id);
    return found === undefined ? { status: 404, body: { error: "x", code: "kb_doc_not_found" } } : { status: 200, body: { doc: found, ...NOTE } };
  });
  server.reply("PATCH", /\/kb\/docs\//, (body, url) => {
    const id = decodeURIComponent(url.split("/").pop() ?? "");
    model.docs = model.docs.map((d) => (d.id === id ? { ...d, ...(body as Partial<KbDoc>) } : d));
    return { status: 200, body: { doc: model.docs.find((d) => d.id === id) } };
  });
  server.reply("DELETE", /\/kb\/docs\//, (_b, url) => {
    const id = decodeURIComponent(url.split("/").pop() ?? "");
    model.docs = model.docs.filter((d) => d.id !== id);
    return { status: 200, body: { removed: id } };
  });
  server.reply("GET", /\/kb\/overview$/, () => ({ status: 200, body: { text: model.overview, max_bytes: 32768 } }));
  server.reply("PUT", /\/kb\/overview$/, (body) => {
    model.overview = (body as { text: string }).text;
    return { status: 200, body: { text: model.overview, max_bytes: 32768 } };
  });
  // A janela carrega a lista ao montar (a rota acima vale a partir da próxima leitura).
  await act(async () => {
    await reloadKb("p1");
  });
  await settle();
  return { ...view, model };
}

function knowledge(): HTMLElement {
  return screen.getByRole("region", { name: "Conhecimento" });
}

function subtitle(): string {
  return document.getElementById(windowSubtitleId("knowledge"))?.textContent ?? "";
}

function row(id: string): HTMLElement {
  const el = knowledge().querySelector<HTMLElement>(`[data-kb-doc="${id}"]`);
  if (el === null) throw new Error(`linha ${id} ausente`);
  return el;
}

beforeEach(() => {
  resetKbStore();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  setLanguage("pt-BR", { persist: false });
});

describe("Conhecimento: lista, estados e subtítulo", () => {
  it("lista com tipo, estado, motivo e indicadores traduzidos, sem marcação; subtítulo vivo com plural", async () => {
    await mount();
    expect(subtitle()).toBe("5 documentos · 1 indexando · 1 com problema");
    expect(row(API).querySelector(".nd-kb-title")?.textContent).toBe("Padrões de API");
    expect(knowledge().querySelector("[data-kb-role]")).toBeNull();
    expect(row(API).textContent).toContain("padroes-api.md · 2 KB");
    expect(row(API).querySelector('[data-kb-status="ready"]')?.textContent).toBe("Pronto");
    expect(row(LINK).textContent).not.toContain("Referência");
    expect(row(LINK).textContent).toContain('O link interno para "banco.md" não aponta para nenhum documento da base');
    expect(row(GATEWAY).querySelector('[data-kb-status="extracting"]')?.textContent).toBe("Indexando");
    expect(row(SCAN).textContent).toContain("Sem texto aproveitável");
    expect(row(COPY).textContent).toContain('Mesmo conteúdo de "Padrões de API"');
  });

  it("um documento e nenhum indexando: singular", async () => {
    await mount({ list: [docs()[0] as KbDoc] });
    expect(subtitle()).toBe("1 documento");
  });

  it("vazia: o convite com Adicionar arquivos, o link e o aviso de que fica fora do Git", async () => {
    await mount({ list: [] });
    const win = knowledge();
    expect(win.querySelector("[data-kb-empty]")?.textContent).toContain("A base de conhecimento está vazia");
    expect(within(win).getByRole("button", { name: "Adicionar arquivos" })).toBeTruthy();
    expect(within(win).getByLabelText("Link de uma página")).toBeTruthy();
    expect(win.textContent).toContain("fora do Git do projeto");
    expect(subtitle()).toBe("0 documentos");
  });

  it("em inglês, sem português da interface", async () => {
    setLanguage("en", { persist: false });
    await mount();
    expect(subtitle()).toBe("5 documents · 1 indexing · 1 with a problem");
    const win = screen.getByRole("region", { name: "Knowledge" });
    expect(win.querySelector(`[data-kb-doc="${COPY}"]`)?.textContent).toContain('Same content as "Padrões de API"');
    expect(within(win).getByRole("button", { name: "Add files" })).toBeTruthy();
  });

  it("kb_updated relê a lista e o subtítulo acompanha", async () => {
    const { model, stream, server } = await mount();
    const before = server.count("GET", /\/api\/projects\/p1\/kb$/);
    model.docs = model.docs.map((d) => (d.id === GATEWAY ? { ...d, status: "ready" } : d));
    await act(async () => {
      const event = { event_type: "kb_updated", project_id: "p1", payload: { id: GATEWAY, status: "ready", status_reason: null, doc: { id: GATEWAY } } } as unknown as ApiEvent;
      stream().emit("kb_updated", event);
    });
    await settle();
    expect(server.count("GET", /\/api\/projects\/p1\/kb$/)).toBeGreaterThan(before);
    expect(subtitle()).toBe("5 documentos · 1 com problema");
  });
});

describe("Conhecimento: envio e link", () => {
  it("vários arquivos pelo botão: um POST octet-stream por arquivo; recusa por código traduzida", async () => {
    const { server } = await mount();
    server.reply("POST", /\/kb\/files\?name=/, (_b, url) =>
      url.includes(".env") ? { status: 422, body: { error: "x", code: "kb_secret_name" } } : { status: 201, body: { doc: doc({ id: "novo-1234abcd", title: "Novo", kind: "markdown" }) } },
    );
    const input = knowledge().querySelector<HTMLInputElement>('input[type="file"]');
    if (input === null) throw new Error("campo de arquivo ausente");
    const files = [new File(["# A"], "a.md", { type: "text/markdown" }), new File(["X=1"], ".env")];
    Object.defineProperty(input, "files", { value: files, configurable: true });
    await act(async () => {
      fireEvent.change(input);
    });
    await settle();
    expect(server.calls.filter((c) => c.method === "POST" && /\/kb\/files\?name=a\.md$/.test(c.url))).toHaveLength(1);
    expect(server.calls.filter((c) => c.method === "POST" && /\/kb\/files\?name=\.env$/.test(c.url))).toHaveLength(1);
    expect(within(knowledge()).getByRole("alert").textContent).toBe(".env: nome típico de arquivo de segredo (.env, chave PEM e afins); não foi adicionado");
  });

  it("arrastar e soltar envia os arquivos", async () => {
    const { server } = await mount();
    server.reply("POST", /\/kb\/files\?name=/, () => ({ status: 201, body: { doc: doc({ id: "novo-1234abcd", title: "Novo", kind: "markdown" }) } }));
    const drop = knowledge().querySelector<HTMLElement>("[data-kb-drop]");
    if (drop === null) throw new Error("área de soltar ausente");
    const file = new File(["# B"], "b.md");
    await act(async () => {
      fireEvent.drop(drop, { dataTransfer: { files: [file], types: ["Files"] } });
    });
    await settle();
    expect(server.count("POST", /\/kb\/files\?name=b\.md$/)).toBe(1);
  });

  it("link: POST com o endereço; o erro por código aparece traduzido", async () => {
    const { server } = await mount();
    let posted: unknown = null;
    server.reply("POST", /\/kb\/links$/, (body) => {
      posted = body;
      return { status: 422, body: { error: "x", code: "kb_url_invalid" } };
    });
    fireEvent.change(within(knowledge()).getByLabelText("Link de uma página"), { target: { value: "notaurl" } });
    await act(async () => {
      fireEvent.click(within(knowledge()).getByRole("button", { name: "Adicionar link" }));
    });
    await settle();
    expect(posted).toEqual({ url: "notaurl" });
    expect(within(knowledge()).getByRole("alert").textContent).toBe("link inválido: use um endereço http ou https completo");
  });
});

describe("Conhecimento: prévia e edição", () => {
  it("abre a nota, edita a descrição e remove com confirmação, sem marcação nem Vale para", async () => {
    const { server } = await mount();
    const bodies: unknown[] = [];
    const patch = server.calls.length;
    await act(async () => {
      fireEvent.click(within(row(API)).getByRole("button"));
    });
    await settle();
    const preview = (): HTMLElement => {
      const el = knowledge().querySelector<HTMLElement>(`[data-kb-preview="${API}"]`);
      if (el === null) throw new Error("prévia ausente");
      return el;
    };
    expect(preview().textContent).toContain("Prefixo /v2.");

    fireEvent.click(within(preview()).getByRole("button", { name: "Editar Descrição" }));
    fireEvent.change(within(preview()).getByLabelText("Descrição"), { target: { value: "  Rotas, erros e paginação  " } });
    await act(async () => {
      fireEvent.click(within(preview()).getByRole("button", { name: "Salvar" }));
    });
    await settle();
    for (const c of server.calls.slice(patch)) if (c.method === "PATCH") bodies.push(c.body);
    expect(bodies).toEqual([{ description: "Rotas, erros e paginação" }]);
    // Spec 136 T02: sem o seletor Padrão ou Referência, sem o "Vale para" e sem as dicas deles.
    for (const gone of ["Padrão do projeto", "Referência", "Marcação", "Vale para", "Todo o projeto"]) expect(preview().textContent).not.toContain(gone);
    expect(within(preview()).queryByRole("group", { name: "Marcação" })).toBeNull();
    expect(within(preview()).queryByRole("button", { name: "Editar Vale para" })).toBeNull();
    expect(preview().querySelectorAll("[data-kb-field]").length).toBe(1);

    fireEvent.click(within(preview()).getByRole("button", { name: "Remover Padrões de API" }));
    expect(server.count("DELETE", /\/kb\/docs\//)).toBe(0);
    const confirm = knowledge().querySelector<HTMLElement>("[data-kb-confirm]");
    if (confirm === null) throw new Error("confirmação ausente");
    expect(confirm.textContent).toContain('Remover "Padrões de API" da base?');
    await act(async () => {
      fireEvent.click(within(confirm).getByRole("button", { name: "Remover" }));
    });
    await settle();
    expect(server.count("DELETE", new RegExp(`/kb/docs/${API}$`))).toBe(1);
    expect(knowledge().querySelector(`[data-kb-doc="${API}"]`)).toBeNull();
    expect(subtitle()).toBe("4 documentos · 1 indexando · 1 com problema");
  });

  it("erro do PATCH por código, traduzido", async () => {
    const { server } = await mount();
    server.reply("PATCH", /\/kb\/docs\//, () => ({ status: 422, body: { error: "x", code: "kb_description_too_long", params: { max: 200 } } }));
    await act(async () => {
      fireEvent.click(within(row(API)).getByRole("button"));
    });
    await settle();
    const preview = knowledge().querySelector<HTMLElement>(`[data-kb-preview="${API}"]`);
    if (preview === null) throw new Error("prévia ausente");
    fireEvent.click(within(preview).getByRole("button", { name: "Editar Descrição" }));
    await act(async () => {
      fireEvent.click(within(preview).getByRole("button", { name: "Salvar" }));
    });
    await settle();
    expect(within(preview).getByRole("alert").textContent).toBe("a descrição deve ter no máximo 200 caracteres");
  });

  it("Visão geral: lê, edita e grava com PUT", async () => {
    const { server, model } = await mount();
    await act(async () => {
      fireEvent.click(within(knowledge()).getByRole("button", { name: "Visão geral" }));
    });
    await settle();
    const field = within(knowledge()).getByLabelText("Visão geral") as HTMLTextAreaElement;
    expect(field.value).toBe("Resumo do projeto.");
    // A ajuda diz que é ali que se declaram os padrões da empresa, lidos como a intenção da pessoa.
    const hint = knowledge().querySelector(`#${CSS.escape(field.getAttribute("aria-describedby") ?? "")}`);
    expect(hint?.textContent).toContain("quais documentos são padrão da empresa");
    expect(hint?.textContent).toContain("como a sua intenção");
    fireEvent.change(field, { target: { value: "API de pagamentos." } });
    await act(async () => {
      fireEvent.click(within(knowledge()).getByRole("button", { name: "Salvar" }));
    });
    await settle();
    expect(server.count("PUT", /\/kb\/overview$/)).toBe(1);
    expect(model.overview).toBe("API de pagamentos.");
    expect(knowledge().textContent).toContain("Visão geral salva.");
  });
});

describe("Plano: citação kb: abre o Conhecimento", () => {
  const spec: SpecView = {
    ...makeSpec("s1", "Estorno", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]),
    premises: [`A rota segue kb:${API}#versionamento. Não é citação: kb:session e kb:user:1.`],
  };

  it("só o id real vira link; o clique abre a janela no documento com a seção em destaque", async () => {
    await mount({ spec, layout: { layouts: { planning: { current: { windows: { plan: { x: 0, y: 0, w: 700, h: 700, z: 1, mode: "normal", open: true } } } } } } });
    const plan = screen.getByRole("region", { name: "Plano" });
    const details = plan.querySelector<HTMLDetailsElement>("details[data-plan-more]");
    if (details !== null) details.open = true;
    const links = plan.querySelectorAll("[data-kb-cite]");
    expect([...links].map((l) => l.getAttribute("data-kb-cite"))).toEqual([`kb:${API}#versionamento`]);
    expect(plan.textContent).toContain("kb:session");
    await act(async () => {
      fireEvent.click(links[0] as Element);
    });
    await settle();
    const preview = knowledge().querySelector<HTMLElement>(`[data-kb-preview="${API}"]`);
    if (preview === null) throw new Error("prévia ausente");
    const cited = preview.querySelector<HTMLElement>('[data-kb-cited="versionamento"]');
    expect(cited?.textContent).toContain("Seção citada no plano");
    expect(cited?.textContent).toContain("Prefixo /v2.");
    expect(cited?.textContent).not.toContain("problem+json");
  });
});

describe("Navegação (spec 136 T07)", () => {
  async function openApi() {
    const view = await mount();
    await act(async () => {
      fireEvent.click(within(row(API)).getByRole("button"));
    });
    await settle();
    const preview = knowledge().querySelector<HTMLElement>(`[data-kb-preview="${API}"]`);
    if (preview === null) throw new Error("prévia ausente");
    return { ...view, preview };
  }

  /** O sumário, aberto se estiver recolhido (janela média ou estreita). */
  function outline(): HTMLElement {
    const nav = within(knowledge()).getByRole("navigation", { name: "Sumário" });
    const toggle = nav.querySelector<HTMLButtonElement>(".nd-kb-outline-toggle");
    if (toggle !== null && toggle.getAttribute("aria-expanded") === "false") fireEvent.click(toggle);
    return nav;
  }

  it("/ fora de um campo leva à busca rápida; dentro de um campo, é só a letra", async () => {
    await mount();
    const search = within(knowledge()).getByRole("combobox", { name: "Buscar na base de conhecimento" });
    const link = within(knowledge()).getByLabelText("Link de uma página");
    link.focus();
    fireEvent.keyDown(link, { key: "/" });
    expect(document.activeElement).toBe(link);
    const rowButton = within(row(API)).getByRole("button");
    rowButton.focus();
    fireEvent.keyDown(rowButton, { key: "/" });
    expect(document.activeElement).toBe(search);
  });

  it("o sumário lista as seções sem o título do documento e o pulo destaca a seção", async () => {
    const { preview } = await openApi();
    const nav = outline();
    const items = within(nav).getAllByRole("button").filter((b) => b.classList.contains("nd-kb-outline-item"));
    expect(items.map((b) => b.textContent)).toEqual(["Versionamento", "Erros"]);
    await act(async () => {
      fireEvent.click(within(nav).getByRole("button", { name: "Erros" }));
    });
    const focus = preview.querySelector<HTMLElement>('[data-kb-focus="erros"]');
    expect(focus?.textContent).toContain("problem+json.");
    expect(focus?.textContent).not.toContain("Prefixo /v2.");
    expect(focus?.getAttribute("data-flash")).toBe("true");
    expect(focus?.textContent).not.toContain("Seção citada no plano");
    expect(within(outline()).getByRole("button", { name: "Erros" }).getAttribute("aria-current")).toBe("location");
  });

  it("a seção em leitura acompanha a rolagem da nota", async () => {
    const { preview } = await openApi();
    const tops: Record<string, number> = { "padroes-de-api": -400, versionamento: 10, erros: 300 };
    vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
      const sec = (this as HTMLElement).dataset?.kbSec;
      const top = sec === undefined ? 0 : (tops[sec] ?? 0);
      return { top, bottom: top + 10, left: 0, right: 0, width: 0, height: 10, x: 0, y: top, toJSON: () => ({}) } as DOMRect;
    });
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      cb(0);
      return 1;
    });
    await act(async () => {
      fireEvent.scroll(preview);
    });
    expect(within(outline()).getByRole("button", { name: "Versionamento" }).getAttribute("aria-current")).toBe("location");
    tops.erros = 20;
    await act(async () => {
      fireEvent.scroll(preview);
    });
    expect(within(outline()).getByRole("button", { name: "Erros" }).getAttribute("aria-current")).toBe("location");
  });
});

describe("Conhecimento: documentos escritos no Nants (spec 136 T11)", () => {
  function preview(id: string): HTMLElement {
    const el = knowledge().querySelector<HTMLElement>(`[data-kb-preview="${id}"]`);
    if (el === null) throw new Error("prévia ausente");
    return el;
  }

  async function open(id: string): Promise<void> {
    await act(async () => {
      fireEvent.click(within(row(id)).getByRole("button"));
    });
    await settle();
  }

  it("Editar abre o editor com o original (não a nota), e Concluir volta à prévia", async () => {
    const { server } = await mount();
    server.reply("GET", /\/kb\/docs\/[^/]+\/content$/, () => ({ status: 200, body: { content: "---\nfonte: equipe\n---\n# Padrões de API\n", sha256: "sha-api" } }));
    await open(API);
    const edit = within(preview(API)).getByRole("button", { name: 'Editar "Padrões de API"' });
    expect(edit.textContent).toBe("Editar");
    // O "Editar" da descrição diz o que edita.
    expect(within(preview(API)).getByRole("button", { name: "Editar Descrição" }).textContent).toBe("Editar descrição");
    await act(async () => {
      fireEvent.click(edit);
    });
    await settle();
    const area = (await within(knowledge()).findByRole("textbox", { name: "Texto do documento, em markdown" }, { timeout: 10_000 })) as HTMLTextAreaElement;
    await settle();
    expect(area.value).toBe("---\nfonte: equipe\n---\n# Padrões de API\n");
    expect(server.count("GET", /\/kb\/docs\/padroes-api-3f9a1c2e\/content$/)).toBe(1);
    // Em edição, a janela foca na escrita: sem a lista, a busca e as abas.
    expect(knowledge().querySelector("[data-kb-side]")).toBeNull();
    expect(within(knowledge()).queryByRole("button", { name: "Documentos" })).toBeNull();
    await act(async () => {
      fireEvent.click(within(knowledge()).getByRole("button", { name: "Concluir" }));
    });
    await settle();
    expect(preview(API)).toBeTruthy();
  });

  it("Novo documento abre o editor vazio, com o campo do título", async () => {
    await mount();
    await act(async () => {
      fireEvent.click(within(knowledge()).getByRole("button", { name: "Novo documento" }));
    });
    await settle();
    const title = (await within(knowledge()).findByRole("textbox", { name: "Título do documento" }, { timeout: 10_000 })) as HTMLInputElement;
    expect(title.value).toBe("");
    expect(title.placeholder).toBe("Sem título");
    expect(knowledge().querySelector("[data-state]")?.textContent).toBe("Falta o título");
  });

  it("texto acima de 2 MiB fica só leitura, com o motivo; PDF e link não têm Editar", async () => {
    const big = { id: "registro-1a2b3c4d", title: "Registro legado", kind: "text" as const, file_name: "registro.txt", size: 3 * 1024 * 1024 };
    await mount({ list: [...docs(), doc(big)] });
    await open(big.id);
    expect(within(preview(big.id)).queryByRole("button", { name: /^Editar "/ })).toBeNull();
    expect(preview(big.id).querySelector("[data-kb-readonly]")?.textContent).toBe("Só leitura");
    expect(preview(big.id).textContent).toContain("Arquivos acima de 2 MiB não são editados no Nants.");
    await open(SCAN);
    expect(within(preview(SCAN)).queryByRole("button", { name: /^Editar "/ })).toBeNull();
    expect(preview(SCAN).querySelector("[data-kb-readonly]")).toBeNull();
  });
});
