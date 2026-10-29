/*
 * Menções com `@` na Conversa (spec 136 T06, critérios 2 a 5 e 7): o seletor da base no campo, o
 * filtro sem acento, o disparo (inclusive `@RestController` e `@types/node` como texto), o teclado,
 * o toque sem tirar o foco, o passo da seção, a etiqueta e o Backspace, o envio com `kb:`, o
 * histórico com a etiqueta, o rascunho refeito e a acessibilidade.
 */
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { KbDoc, KbSection } from "../../api/kb";
import type { ChatHistoryTurn } from "../../api/types";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { makeProject, makeProjectState } from "../../test-utils";
import { resetKbExcerpts } from "../knowledge/kbCiteStore";
import { reloadKb, resetKbStore } from "../knowledge/kbStore";

const GUIA = "guia-de-estilo-3fa9c2e1";
const REST = "padroes-rest-8b1d07aa";
const JAVA = "padroes-de-projeto-77e0f3b2";
const H2 = "manual-do-h2-9c8b7a65";

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
  doc(GUIA, "Guia de estilo da API", { file_name: "guia.md" }),
  doc(REST, "Padrões REST da equipe", { file_name: "padroes-rest.md" }),
  doc(JAVA, "Os padrões de projeto", { kind: "pdf", file_name: "projeto.pdf" }),
  doc(H2, "Manual do H2", { status: "extracting" }),
];

const SECTIONS: KbSection[] = [
  { anchor: "visao-geral", title: "Visão geral", level: 2, line: 2 },
  { anchor: "tratamento-de-erros", title: "Tratamento de erros", level: 2, line: 5 },
  { anchor: "erros-de-validacao", title: "Erros de validação", level: 3, line: 8 },
];

const CHAT = /^\/api\/projects\/p1\/chat$/;

beforeEach(() => {
  resetKbStore();
  resetKbExcerpts();
});

afterEach(async () => {
  // O rascunho fica guardado por projeto: cada teste começa com o campo vazio.
  const area = document.querySelector<HTMLTextAreaElement>("textarea.nd-composer-input");
  if (area !== null) {
    fireEvent.change(area, { target: { value: "" } });
    await settle();
  }
  cleanup();
});

async function mount(options: { docs?: KbDoc[]; chat?: ChatHistoryTurn[] } = {}) {
  const view = await renderShell({
    url: "/?space=p1",
    projects: [makeProject("p1", "Agenda")],
    states: { p1: makeProjectState({ id: "p1", name: "Agenda" }) },
    ...(options.chat !== undefined ? { chat: { p1: options.chat } } : {}),
  });
  const { server } = view;
  server.reply("GET", /\/api\/projects\/p1\/kb$/, () => ({ status: 200, body: { docs: options.docs ?? DOCS, has_overview: false } }));
  server.reply("GET", /\/kb\/docs\/[^/?]+$/, (_b, url) => {
    const id = decodeURIComponent(url.split("/").pop() ?? "");
    const found = (options.docs ?? DOCS).find((d) => d.id === id);
    return found === undefined ? { status: 404, body: { error: "x", code: "serverError.kb_not_found" } } : { status: 200, body: { doc: found, content: "# x", sections: id === GUIA ? SECTIONS : [] } };
  });
  server.reply("POST", CHAT, () => ({ status: 200, body: { reply: "Entendido.", draft_updated: false, turn_id: "a1" } }));
  // O rascunho com `@` ou `kb:` lê a base na montagem, antes destas rotas: relê com elas.
  await act(async () => {
    await reloadKb("p1");
  });
  await settle();
  return view;
}

function field(): HTMLTextAreaElement {
  const conversation = screen.getByRole("region", { name: "Conversa" });
  return within(conversation).getByRole("textbox", { name: "Mensagem para o Arquiteto" }) as HTMLTextAreaElement;
}

async function type(text: string): Promise<void> {
  const area = field();
  act(() => area.focus());
  fireEvent.change(area, { target: { value: text } });
  await settle();
}

async function key(name: string, init: Partial<KeyboardEventInit> = {}): Promise<boolean> {
  let notPrevented = true;
  await act(async () => {
    notPrevented = fireEvent.keyDown(field(), { key: name, ...init });
  });
  await settle();
  return notPrevented;
}

function list(): HTMLElement | null {
  return document.querySelector<HTMLElement>("[data-kb-picker] [role='listbox']");
}

function options(): HTMLElement[] {
  return [...(list()?.querySelectorAll<HTMLElement>("[role='option']") ?? [])];
}

function optionTitles(): string[] {
  return options().map((o) => o.querySelector(".nd-kb-picker-title")?.textContent ?? "");
}

function selected(): string | null {
  const id = field().getAttribute("aria-activedescendant");
  return id === null ? null : (document.getElementById(id)?.querySelector(".nd-kb-picker-title")?.textContent ?? null);
}

function sentMessages(server: Awaited<ReturnType<typeof mount>>["server"]): string[] {
  return server.calls.filter((c) => c.method === "POST" && CHAT.test(c.url)).map((c) => (c.body as { message: string }).message);
}

describe("seletor no @", () => {
  it("abre com o filtro sem acento, na ordem, com ícone, título e origem; a primeira opção destacada", async () => {
    await mount();
    await type("siga os @PADROES");
    expect(list()?.getAttribute("aria-label")).toBe("Documentos da base");
    expect(optionTitles()).toEqual(["Padrões REST da equipe", "Os padrões de projeto"]);
    expect(options()[0]?.querySelector(".nd-kb-picker-hit")?.textContent).toBe("Padrões");
    expect(options()[0]?.querySelector(".nd-kb-picker-origin")?.textContent).toBe("padroes-rest.md");
    expect(options()[0]?.querySelector(".nd-kb-picker-icon svg")).not.toBeNull();
    expect(selected()).toBe("Padrões REST da equipe");
    expect(field().getAttribute("aria-controls")).toBe(list()?.id);
    expect(field().getAttribute("aria-autocomplete")).toBe("list");
    expect(screen.getByText("2 documentos", { selector: "[role='status']" })).toBeTruthy();
  });

  it("o documento ainda não pronto não aparece", async () => {
    await mount();
    await type("@manual");
    expect(list()).toBeNull();
  });

  it("`@RestController` e `@types/node` como texto: a lista fecha e o Enter envia", async () => {
    const { server } = await mount();
    await type("anote com @RestController");
    expect(list()).toBeNull();
    expect(field().getAttribute("aria-activedescendant")).toBeNull();
    await key("Enter");
    await type("instale @types/node");
    expect(list()).toBeNull();
    await key("Enter");
    expect(sentMessages(server)).toEqual(["anote com @RestController", "instale @types/node"]);
  });

  it("`@` sozinho abre a lista sem destaque, e o Enter envia", async () => {
    const { server } = await mount();
    await type("siga @");
    expect(optionTitles()).toEqual(["Guia de estilo da API", "Padrões REST da equipe", "Os padrões de projeto"]);
    expect(selected()).toBeNull();
    await key("Enter");
    expect(sentMessages(server)).toEqual(["siga @"]);
  });

  it("no meio de palavra (e-mail) não abre", async () => {
    await mount();
    await type("mande para ana@guia");
    expect(list()).toBeNull();
  });

  it("base sem documento pronto: nada abre e o campo não anuncia a lista", async () => {
    await mount({ docs: [doc(H2, "Manual do H2", { status: "extracting" })] });
    await type("@");
    expect(list()).toBeNull();
    expect(field().getAttribute("aria-autocomplete")).toBeNull();
  });
});

describe("teclado e toque", () => {
  it("setas movem o destaque (com volta), Enter escolhe sem enviar e a etiqueta entra no campo", async () => {
    const { server } = await mount();
    await type("siga @");
    await key("ArrowDown");
    expect(selected()).toBe("Guia de estilo da API");
    await key("ArrowUp");
    expect(selected()).toBe("Os padrões de projeto");
    await key("ArrowDown");
    expect(await key("Enter")).toBe(false);
    expect(field().value).toBe("siga @Guia de estilo da API ");
    expect(list()).toBeNull();
    expect(document.querySelector(".nd-mention")?.textContent).toContain("Guia de estilo da API");
    expect(field().getAttribute("data-mentions")).toBe("true");
    expect(sentMessages(server)).toEqual([]);
  });

  it("Esc fecha e a lista não volta enquanto se escreve a mesma menção", async () => {
    await mount();
    await type("@gui");
    expect(list()).not.toBeNull();
    expect(await key("Escape")).toBe(false);
    expect(list()).toBeNull();
    await type("@guia");
    expect(list()).toBeNull();
    await type("@guia e @pad");
    expect(list()).not.toBeNull();
  });

  it("Tab escolhe o documento sem seções; Shift+Tab e outras teclas ficam com o campo", async () => {
    await mount({ docs: [doc(REST, "Padrões REST", { indicators: [{ code: "no_headings" }] })] });
    await type("@pad");
    expect(options()[0]?.querySelector(".nd-kb-picker-more")).toBeNull();
    expect(await key("Tab", { shiftKey: true })).toBe(true);
    expect(await key("Tab")).toBe(false);
    expect(field().value).toBe("@Padrões REST ");
  });

  it("o toque e o clique não tiram o foco do campo e escolhem", async () => {
    await mount();
    await type("@gui");
    const option = options()[0] as HTMLElement;
    expect(fireEvent.pointerDown(option)).toBe(false);
    expect(fireEvent.mouseDown(option)).toBe(false);
    await act(async () => {
      fireEvent.click(option);
    });
    await settle();
    expect(field().value).toBe("@Guia de estilo da API ");
    expect(document.activeElement).toBe(field());
  });
});

describe("passo da seção", () => {
  it("Tab abre o sumário em níveis, a seta para a esquerda volta e a direita entra de novo", async () => {
    await mount();
    await type("siga @guia");
    expect(await key("Tab")).toBe(false);
    expect(field().value).toBe("siga @Guia de estilo da API #");
    await waitFor(() => expect(optionTitles()).toEqual(["Documento inteiro", "#Visão geral", "#Tratamento de erros", "#Erros de validação"]));
    expect(list()?.getAttribute("aria-label")).toBe('Seções de "Guia de estilo da API"');
    expect(options()[3]?.getAttribute("data-depth")).toBe("1");
    expect(selected()).toBe("Documento inteiro");
    await key("ArrowLeft");
    expect(field().value).toBe("siga @guia");
    expect(list()?.getAttribute("aria-label")).toBe("Documentos da base");
    await key("ArrowRight");
    expect(field().value).toBe("siga @Guia de estilo da API #");
  });

  it("o texto depois do # filtra as seções; Enter põe a etiqueta da seção e o envio leva a âncora", async () => {
    const { server } = await mount();
    await type("siga @guia");
    await key("Tab");
    await type("siga @Guia de estilo da API #valid");
    expect(optionTitles()).toEqual(["#Erros de validação"]);
    await key("Enter");
    expect(field().value).toBe("siga @Guia de estilo da API #Erros de validação ");
    await type(`${field().value}no formulário`);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
    });
    await settle();
    expect(sentMessages(server)).toEqual([`siga kb:${GUIA}#erros-de-validacao no formulário`]);
  });

  it("a seta da linha abre as seções pelo toque, e o documento inteiro vai sem âncora", async () => {
    const { server } = await mount();
    await type("@guia");
    await act(async () => {
      fireEvent.click(options()[0]?.querySelector(".nd-kb-picker-more") as Element);
    });
    await settle();
    expect(field().value).toBe("@Guia de estilo da API #");
    await key("Enter");
    expect(field().value).toBe("@Guia de estilo da API ");
    await key("Enter");
    expect(sentMessages(server)).toEqual([`kb:${GUIA}`]);
  });
});

describe("passo da seção: títulos difíceis e o Backspace", () => {
  const LONG = `Spring Framework Reference Documentation ${"capítulo ".repeat(7)}final`;
  const AT = "Contato @time de plataforma";

  it("título de 100 caracteres: o sumário abre, filtra e a seção entra", async () => {
    expect(LONG.length).toBeGreaterThan(80);
    const { server } = await mount({ docs: [doc(GUIA, LONG)] });
    await type("@spring");
    await key("Tab");
    await waitFor(() => expect(optionTitles()).toContain("#Erros de validação"));
    await type(`${field().value}valid`);
    expect(optionTitles()).toEqual(["#Erros de validação"]);
    await key("Enter");
    await key("Enter");
    expect(sentMessages(server)).toEqual([`kb:${GUIA}#erros-de-validacao`]);
  });

  it("título com @: o sumário abre e a seção entra", async () => {
    const { server } = await mount({ docs: [doc(GUIA, AT)] });
    await type("@contato");
    await key("Tab");
    expect(field().value).toBe(`@${AT} #`);
    await waitFor(() => expect(optionTitles()).toContain("#Visão geral"));
    await key("ArrowDown");
    await key("Enter");
    expect(field().value).toBe(`@${AT} #Visão geral `);
    await key("Enter");
    expect(sentMessages(server)).toEqual([`kb:${GUIA}#visao-geral`]);
  });

  it("Backspace com a consulta da seção vazia volta aos documentos, com a consulta de antes", async () => {
    await mount();
    await type("siga @gui");
    await key("Tab");
    expect(field().value).toBe("siga @Guia de estilo da API #");
    expect(await key("Backspace")).toBe(false);
    expect(field().value).toBe("siga @gui");
    expect(list()?.getAttribute("aria-label")).toBe("Documentos da base");
    expect(selected()).toBe("Guia de estilo da API");
  });
});

describe("etiqueta, envio, histórico e rascunho", () => {
  it("o pedido do lançador troca o texto: as etiquetas antigas saem e o envio não leva kb:", async () => {
    const { server } = await mount();
    await type("@gui");
    await key("Enter");
    expect(field().getAttribute("data-mentions")).toBe("true");
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    const launcher = screen.getByRole("dialog", { name: "Lançador" });
    fireEvent.change(within(launcher).getByRole("combobox"), { target: { value: "@Guia de estilo da API agora" } });
    await act(async () => {
      fireEvent.click(within(launcher).getByRole("option", { name: /Pedir ao Arquiteto/ }));
    });
    await settle();
    expect(field().value).toBe("@Guia de estilo da API agora");
    expect(field().getAttribute("data-mentions")).toBeNull();
    expect(document.querySelector(".nd-mention")).toBeNull();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
    });
    await settle();
    expect(sentMessages(server)).toEqual(["@Guia de estilo da API agora"]);
  });

  it("Backspace no fim apaga a etiqueta inteira", async () => {
    await mount();
    await type("siga @gui");
    await key("Enter");
    const area = field();
    const end = "siga @Guia de estilo da API".length;
    act(() => area.setSelectionRange(end, end));
    expect(await key("Backspace")).toBe(false);
    expect(field().value).toBe("siga  ");
    expect(document.querySelector(".nd-mention")).toBeNull();
    expect(field().getAttribute("data-mentions")).toBeNull();
  });

  it("o histórico mostra a menção como etiqueta, com o título da seção", async () => {
    await mount({ chat: [{ role: "user", text: `Siga o kb:${GUIA}#erros-de-validacao e o kb:${REST}.`, ts: 1_790_000_000 }] });
    const bubble = document.querySelector<HTMLElement>(".nd-bubble-user");
    await waitFor(() => expect(bubble?.querySelectorAll("[data-kb-cite]").length).toBe(2));
    await waitFor(() => expect(bubble?.querySelector(`[data-kb-cite="kb:${GUIA}#erros-de-validacao"]`)?.textContent).toBe("Guia de estilo da API#Erros de validação"));
    expect(bubble?.querySelector(`[data-kb-cite="kb:${REST}"]`)?.textContent).toBe("Padrões REST da equipe");
    expect(bubble?.textContent).not.toContain("kb:");
  });

  it("o rascunho guarda o texto codificado e volta com as etiquetas; o documento que saiu fica texto", async () => {
    await mount();
    await type("siga @guia");
    await key("Tab");
    await type("siga @Guia de estilo da API #trat");
    await key("Enter");
    await type(`${field().value}e @pad`);
    await key("Enter");
    cleanup();
    resetKbStore();
    await mount();
    await waitFor(() => expect(field().value).toBe("siga @Guia de estilo da API #Tratamento de erros e @Padrões REST da equipe "));
    expect([...document.querySelectorAll(".nd-mention")].map((m) => m.textContent)).toEqual(["@Guia de estilo da API #Tratamento de erros", "@Padrões REST da equipe"]);
    cleanup();
    resetKbStore();
    resetKbExcerpts();
    await mount({ docs: [DOCS[1] as KbDoc] });
    await waitFor(() => expect(field().value).toBe(`siga kb:${GUIA}#tratamento-de-erros e @Padrões REST da equipe `));
  });
});
