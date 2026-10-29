/*
 * Editor de documentos da base (spec 136 T11, critério 7): a barra e os atalhos, a continuação de
 * lista e o Tab, a arrumação ao salvar, o salvamento automático (espera, estado e nova tentativa),
 * o conflito 409 (Recarregar e Manter o meu), o `[[` com as sugestões, o rascunho local (com o
 * descarte e o rascunho antigo que vira conflito), o documento novo só com o título confirmado e a
 * acessibilidade. O "Novo documento", o "Editar" e o só leitura acima de 2 MiB, na janela, ficam
 * em `Knowledge.window.test.tsx`.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { KbDoc } from "../../../api/kb";
import { installFetch, renderWith, type RouteReply } from "../../../test-utils";
import { resetKbStore } from "../kbStore";
import { KbEditor } from "./KbEditor";
import { clearDraft, readDraft, writeDraft } from "./kbDraft";

const PID = "p1";
const API = "padroes-api-3f9a1c2e";

function doc(partial: Partial<KbDoc> & Pick<KbDoc, "id" | "title">): KbDoc {
  return {
    kind: "markdown",
    source: "written",
    file_name: null,
    source_url: null,
    size: 100,
    sha256: "sha-1",
    added_at: "2026-10-28T12:00:00Z",
    fetched_at: null,
    status: "ready",
    pages: null,
    description: "",
    links_out: [],
    ...partial,
  };
}

const DOCS: KbDoc[] = [
  doc({ id: API, title: "Padrões de API", file_name: "padroes-api.md" }),
  doc({ id: "webhooks-1", title: "Webhooks do WhatsApp", source: "file", file_name: "webhooks whatsapp.md" }),
  doc({ id: "hateoas-2", title: "Spring HATEOAS", kind: "link", source: "url", source_url: "https://exemplo.com/hateoas" }),
  doc({ id: "fila-3", title: "Fila de webhooks", status: "extracting", file_name: "fila.md" }),
];

const ORIGINAL = "# Padrões de API\n\nTexto.\n";

interface Server {
  content: string;
  sha: string;
  puts: { content: string; base_sha256: string }[];
  posts: { title: string; content: string }[];
  put: (body: { content: string; base_sha256: string }) => RouteReply | Promise<RouteReply>;
}

function serve(): Server {
  const server: Server = {
    content: ORIGINAL,
    sha: "sha-1",
    puts: [],
    posts: [],
    put: (body) => {
      server.content = body.content;
      server.sha = `sha-depois-${server.puts.length}`;
      return { status: 200, body: { doc: { ...DOCS[0], sha256: server.sha } } };
    },
  };
  installFetch((method, url, body) => {
    if (method === "GET" && url.endsWith(`/kb/docs/${API}/content`)) return { status: 200, body: { content: server.content, sha256: server.sha } };
    if (method === "PUT" && url.endsWith(`/kb/docs/${API}/content`)) {
      server.puts.push(body as { content: string; base_sha256: string });
      return server.put(body as { content: string; base_sha256: string });
    }
    if (method === "POST" && url.endsWith("/kb/written")) {
      server.posts.push(body as { title: string; content: string });
      return { status: 201, body: { doc: doc({ id: "novo-9", title: (body as { title: string }).title, file_name: "novo.md", sha256: "sha-novo" }) } };
    }
    if (method === "PUT" && url.endsWith("/kb/docs/novo-9/content")) return { status: 200, body: { doc: doc({ id: "novo-9", title: "Novo", sha256: "sha-novo-2" }) } };
    return { status: 404, body: { error: "x", code: "kb_doc_not_found" } };
  });
  return server;
}

async function flush(): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
}

async function wait(ms: number): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

async function mount(options: { doc?: KbDoc | null; stacked?: boolean } = {}) {
  const onDone = vi.fn();
  renderWith(<KbEditor projectId={PID} docs={DOCS} doc={options.doc === undefined ? (DOCS[0] ?? null) : options.doc} stacked={options.stacked ?? false} onDone={onDone} />);
  await flush();
  return { onDone, area: field() };
}

function field(): HTMLTextAreaElement {
  return screen.getByRole("textbox", { name: "Texto do documento, em markdown" }) as HTMLTextAreaElement;
}

function state(): string {
  return document.querySelector("[data-state]")?.textContent ?? "";
}

/** Digita trocando o texto (o cursor vai para o fim, como no jsdom). */
function type(area: HTMLTextAreaElement, value: string): void {
  fireEvent.focus(area);
  fireEvent.change(area, { target: { value } });
}

function select(area: HTMLTextAreaElement, start: number, end = start): void {
  area.setSelectionRange(start, end);
  fireEvent.select(area);
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
  resetKbStore();
  clearDraft(PID, API);
  clearDraft(PID, null);
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  clearDraft(PID, API);
  clearDraft(PID, null);
});

describe("abre o original e a versão", () => {
  it("lê o texto-fonte pela rota de conteúdo, mostra o estado Salvo e a prévia", async () => {
    serve();
    const { area } = await mount();
    expect(area.value).toBe(ORIGINAL);
    expect(state()).toBe("Salvo");
    expect(screen.getByRole("region", { name: "Prévia" }).textContent).toContain("Texto.");
  });

  it("Concluir sem mudança não grava nem arruma o texto", async () => {
    const server = serve();
    server.content = "# T  \n\n\n\nx";
    const { onDone } = await mount();
    fireEvent.click(screen.getByRole("button", { name: "Concluir" }));
    await flush();
    expect(server.puts).toEqual([]);
    expect(onDone).toHaveBeenCalledWith(API);
  });
});

describe("barra e atalhos", () => {
  it("a barra é um toolbar com as ações e os atalhos no título; age na seleção", async () => {
    serve();
    const { area } = await mount();
    const bar = screen.getByRole("toolbar", { name: "Formatação" });
    expect(bar.getAttribute("aria-controls")).toBe(area.id);
    const names = within(bar).getAllByRole("button").map((b) => b.getAttribute("aria-label"));
    expect(names).toEqual(["Título", "Negrito", "Itálico", "Lista", "Lista numerada", "Citação", "Código", "Link", "Link para um documento da base"]);
    expect(within(bar).getByRole("button", { name: "Negrito" }).getAttribute("title")).toBe("Negrito (Ctrl+B)");
    select(area, ORIGINAL.indexOf("Texto"), ORIGINAL.indexOf("Texto") + 5);
    fireEvent.click(within(bar).getByRole("button", { name: "Negrito" }));
    expect(area.value).toBe("# Padrões de API\n\n**Texto**.\n");
    select(area, area.value.indexOf("Texto") + 2);
    fireEvent.click(within(bar).getByRole("button", { name: "Lista" }));
    expect(area.value).toBe("# Padrões de API\n\n- **Texto**.\n");
  });

  it("Ctrl+B, Ctrl+I, Ctrl+E, Ctrl+K, Ctrl+2 e Ctrl+Shift+7 formatam; o Ctrl+K não chega ao lançador", async () => {
    serve();
    const { area } = await mount();
    const launcher = vi.fn();
    window.addEventListener("keydown", launcher);
    const at = ORIGINAL.indexOf("Texto");
    select(area, at, at + 5);
    fireEvent.keyDown(area, { key: "b", ctrlKey: true });
    expect(area.value).toContain("**Texto**");
    select(area, area.value.indexOf("Texto"), area.value.indexOf("Texto") + 5);
    fireEvent.keyDown(area, { key: "i", metaKey: true });
    expect(area.value).toContain("***Texto***");
    select(area, 0);
    fireEvent.keyDown(area, { key: "2", ctrlKey: true });
    expect(area.value.startsWith("## Padrões de API")).toBe(true);
    select(area, area.value.length - 1);
    fireEvent.keyDown(area, { key: "&", code: "Digit7", ctrlKey: true, shiftKey: true });
    expect(area.value).toContain("1. ***Texto***.");
    select(area, area.value.indexOf("Padrões"), area.value.indexOf("Padrões") + 7);
    fireEvent.keyDown(area, { key: "e", ctrlKey: true });
    expect(area.value).toContain("`Padrões`");
    select(area, area.value.indexOf("API"), area.value.indexOf("API") + 3);
    fireEvent.keyDown(area, { key: "k", ctrlKey: true });
    expect(area.value).toContain("[API]()");
    expect(area.selectionStart).toBe(area.value.indexOf("[API]()") + 6);
    expect(launcher).not.toHaveBeenCalled();
    window.removeEventListener("keydown", launcher);
  });

  it("no modo Ver a barra fica desabilitada; os modos são botões com aria-pressed", async () => {
    serve();
    await mount();
    const modes = screen.getByRole("group", { name: "Exibição" });
    expect(within(modes).getAllByRole("button").map((b) => `${b.textContent}:${b.getAttribute("aria-pressed")}`)).toEqual(["Escrever:false", "Lado a lado:true", "Ver:false"]);
    fireEvent.click(within(modes).getByRole("button", { name: "Ver" }));
    expect(screen.queryByRole("toolbar", { name: "Formatação" })).toBeNull();
    expect(screen.queryByRole("textbox", { name: "Texto do documento, em markdown" })).toBeNull();
  });

  it("no celular, só Escrever e Ver, e a barra fica embaixo, sem os atalhos no título", async () => {
    serve();
    await mount({ stacked: true });
    const modes = screen.getByRole("group", { name: "Exibição" });
    expect(within(modes).getAllByRole("button").map((b) => b.textContent)).toEqual(["Escrever", "Ver"]);
    expect(screen.queryByRole("region", { name: "Prévia" })).toBeNull();
    expect(screen.getByRole("button", { name: "Negrito" }).getAttribute("title")).toBe("Negrito");
  });
});

describe("Enter e Tab nas listas", () => {
  it("Enter continua a lista e encerra no item vazio; fora de lista, Enter comum", async () => {
    serve();
    const { area } = await mount();
    type(area, "- um");
    select(area, 4);
    const continued = fireEvent.keyDown(area, { key: "Enter" });
    expect(continued).toBe(false);
    expect(area.value).toBe("- um\n- ");
    fireEvent.keyDown(area, { key: "Enter" });
    expect(area.value).toBe("- um\n");
    type(area, "texto");
    select(area, 5);
    expect(fireEvent.keyDown(area, { key: "Enter" })).toBe(true);
  });

  it("Tab recua o item da lista; fora da lista o Tab segue o foco", async () => {
    serve();
    const { area } = await mount();
    type(area, "- um\n- dois");
    select(area, 9);
    expect(fireEvent.keyDown(area, { key: "Tab" })).toBe(false);
    expect(area.value).toBe("- um\n  - dois");
    fireEvent.keyDown(area, { key: "Tab", shiftKey: true });
    expect(area.value).toBe("- um\n- dois");
    type(area, "texto");
    select(area, 2);
    expect(fireEvent.keyDown(area, { key: "Tab" })).toBe(true);
  });
});

describe("salvamento automático", () => {
  it("espera a pausa, envia o texto arrumado com a versão aberta e mostra Salvando e Salvo", async () => {
    const server = serve();
    let release: (() => void) | null = null;
    const original = server.put;
    server.put = (body) =>
      new Promise((resolve) => {
        release = () => resolve(original(body) as RouteReply);
      });
    const { area } = await mount();
    type(area, "# Padrões de API   \n\n\n\nTexto novo.");
    expect(state()).toBe("Não salvo");
    await wait(1100);
    expect(server.puts).toEqual([]);
    type(area, "# Padrões de API   \n\n\n\nTexto novo!");
    await wait(1100);
    expect(server.puts).toEqual([]);
    await wait(200);
    expect(server.puts).toEqual([{ content: "# Padrões de API\n\nTexto novo!\n", base_sha256: "sha-1" }]);
    expect(state()).toBe("Salvando");
    expect(area.value).toBe("# Padrões de API   \n\n\n\nTexto novo!");
    await act(async () => {
      release?.();
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(state()).toBe("Salvo");
    type(area, "outro");
    await wait(1300);
    expect(server.puts.map((p) => p.base_sha256)).toEqual(["sha-1", "sha-depois-1"]);
  });

  it("falhou: Só neste aparelho, com o motivo, e tenta de novo depois", async () => {
    const server = serve();
    const original = server.put;
    server.put = () => ({ status: 500, body: { error: "x", code: "kb_io_failed" } });
    const { area } = await mount();
    type(area, "texto novo");
    await wait(1300);
    expect(state()).toBe("Só neste aparelho");
    expect(screen.getByRole("alert").textContent).toContain("Não deu para salvar na base");
    expect(readDraft(PID, API)?.content).toBe("texto novo");
    server.put = original;
    await wait(10_000);
    expect(server.puts).toHaveLength(2);
    expect(state()).toBe("Salvo");
    expect(readDraft(PID, API)).toBeNull();
  });
});

describe("gravação em andamento e falhas (revisão 1)", () => {
  it("o que se digita durante um PUT gera outro PUT, e o rascunho fica com a versão nova", async () => {
    const server = serve();
    let release: (() => void) | null = null;
    const original = server.put;
    server.put = (body) =>
      new Promise((resolve) => {
        release = () => resolve(original(body) as RouteReply);
      });
    const { area } = await mount();
    type(area, "primeiro");
    await wait(1300);
    expect(server.puts).toHaveLength(1);
    type(area, "primeiro e segundo");
    await wait(1300);
    expect(server.puts).toHaveLength(1);
    await act(async () => {
      release?.();
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(readDraft(PID, API)).toMatchObject({ content: "primeiro e segundo", base: "sha-depois-1" });
    expect(state()).toBe("Não salvo");
    server.put = original;
    await wait(1300);
    expect(server.puts.map((p) => [p.content, p.base_sha256])).toEqual([
      ["primeiro\n", "sha-1"],
      ["primeiro e segundo\n", "sha-depois-1"],
    ]);
    expect(state()).toBe("Salvo");
    expect(readDraft(PID, API)).toBeNull();
  });

  it("Concluir com o PUT recusado fica aberto, com o motivo à vista, e o 4xx não se repete", async () => {
    const server = serve();
    server.put = () => ({ status: 413, body: { error: "x", code: "kb_written_too_large", params: { max_kib: 2048 } } });
    const { area, onDone } = await mount();
    type(area, "grande demais");
    fireEvent.click(screen.getByRole("button", { name: "Concluir" }));
    await wait(100);
    expect(onDone).not.toHaveBeenCalled();
    expect(screen.getByRole("alert").textContent).toBe("Não foi salvo: o documento deve ter no máximo 2048 KiB. Corrija e o Nants salva de novo; o texto fica guardado neste aparelho.");
    expect(state()).toBe("Não salvo");
    await wait(60_000);
    expect(server.puts).toHaveLength(1);
    expect(readDraft(PID, API)?.content).toBe("grande demais");
  });

  it("Concluir com o conflito aberto não fecha", async () => {
    const server = serve();
    server.put = () => ({ status: 409, body: { error: "x", code: "kb_edit_conflict" } });
    const { area, onDone } = await mount();
    type(area, "meu");
    await wait(1300);
    expect(screen.getByRole("alert").textContent).toContain("Este documento mudou enquanto você escrevia");
    fireEvent.click(screen.getByRole("button", { name: "Concluir" }));
    await wait(100);
    expect(onDone).not.toHaveBeenCalled();
  });

  it("Concluir grava e fecha com o id; com falha de rede, fica aberto e tenta de novo", async () => {
    const server = serve();
    const { area, onDone } = await mount();
    type(area, "ok");
    fireEvent.click(screen.getByRole("button", { name: "Concluir" }));
    await wait(100);
    expect(onDone).toHaveBeenCalledWith(API);
    cleanup();
    const original = server.put;
    server.put = () => ({ status: 503, body: { error: "x", code: "kb_io_failed" } });
    const second = await mount();
    type(second.area, "de novo");
    fireEvent.click(screen.getByRole("button", { name: "Concluir" }));
    await wait(100);
    expect(second.onDone).not.toHaveBeenCalled();
    expect(state()).toBe("Só neste aparelho");
    server.put = original;
    await wait(10_000);
    expect(state()).toBe("Salvo");
  });

  it("título repetido no documento novo: o campo do título volta editável e com o foco", async () => {
    const server = serve();
    installFetch((method, url, body) => {
      if (method === "POST" && url.endsWith("/kb/written")) {
        server.posts.push(body as { title: string; content: string });
        return { status: 409, body: { error: "x", code: "kb_title_taken", params: { id: API } } };
      }
      return { status: 404, body: { error: "x", code: "kb_doc_not_found" } };
    });
    await mount({ doc: null });
    const title = screen.getByRole("textbox", { name: "Título do documento" });
    fireEvent.change(title, { target: { value: "Padrões de API" } });
    fireEvent.keyDown(title, { key: "Enter" });
    fireEvent.change(field(), { target: { value: "texto" } });
    await wait(1300);
    expect(server.posts).toHaveLength(1);
    expect(screen.getByRole("alert").textContent).toContain("já existe um documento com este título");
    expect(document.activeElement).toBe(title);
    expect(state()).toBe("Falta o título");
    await wait(60_000);
    expect(server.posts).toHaveLength(1);
  });

  it("Tab no documento vazio ou numa linha em branco segue o foco e não apaga nada", async () => {
    serve();
    const { area } = await mount();
    type(area, "");
    expect(fireEvent.keyDown(area, { key: "Tab" })).toBe(true);
    type(area, "um\n\n");
    select(area, 4);
    expect(fireEvent.keyDown(area, { key: "Tab" })).toBe(true);
    expect(area.value).toBe("um\n\n");
  });
});

describe("conflito", () => {
  function conflicting(server: Server): void {
    server.put = (body) => {
      if (body.base_sha256 !== server.sha) return { status: 409, body: { error: "x", code: "kb_edit_conflict" } };
      server.content = body.content;
      return { status: 200, body: { doc: { ...DOCS[0], sha256: "sha-final" } } };
    };
  }

  it("o 409 relê o original e oferece Recarregar, que troca o texto pela versão atual", async () => {
    const server = serve();
    conflicting(server);
    const { area } = await mount();
    server.content = "# Deles\n";
    server.sha = "sha-deles";
    type(area, "# Meu\n");
    await wait(1300);
    const alert = screen.getByRole("alert");
    expect(alert.textContent).toContain("Este documento mudou enquanto você escrevia");
    fireEvent.click(within(alert).getByRole("button", { name: "Recarregar" }));
    expect(area.value).toBe("# Deles\n");
    expect(state()).toBe("Salvo");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("Manter o meu grava o meu texto sobre a versão atual", async () => {
    const server = serve();
    conflicting(server);
    const { area } = await mount();
    server.content = "# Deles\n";
    server.sha = "sha-deles";
    type(area, "# Meu\n");
    await wait(1300);
    fireEvent.click(within(screen.getByRole("alert")).getByRole("button", { name: "Manter o meu" }));
    await wait(0);
    await flush();
    expect(server.puts.at(-1)).toEqual({ content: "# Meu\n", base_sha256: "sha-deles" });
    expect(server.content).toBe("# Meu\n");
    expect(state()).toBe("Salvo");
  });
});

describe("links com [[", () => {
  it("sugere os documentos prontos da base e insere o link markdown relativo", async () => {
    serve();
    const { area } = await mount();
    type(area, "Veja os [[web");
    const list = screen.getByRole("listbox", { name: "Documentos da base" });
    const options = within(list).getAllByRole("option").map((o) => o.textContent ?? "");
    expect(options).toHaveLength(1);
    expect(options[0]).toContain("Webhooks do WhatsApp");
    expect(area.getAttribute("aria-controls")).toBe(list.id);
    expect(area.getAttribute("aria-activedescendant")).toBe(within(list).getAllByRole("option")[0]?.id);
    expect(screen.getByText("inserir o link")).toBeTruthy();
    fireEvent.keyDown(area, { key: "Enter" });
    expect(area.value).toBe("Veja os [Webhooks do WhatsApp](webhooks%20whatsapp.md)");
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("documento de link vira o endereço; Esc fecha as sugestões", async () => {
    serve();
    const { area } = await mount();
    type(area, "[[hateoas");
    fireEvent.click(screen.getByRole("option", { name: /Spring HATEOAS/ }));
    expect(area.value).toBe("[Spring HATEOAS](https://exemplo.com/hateoas)");
    type(area, "x [[pad");
    expect(screen.queryByRole("listbox")).not.toBeNull();
    fireEvent.keyDown(area, { key: "Escape" });
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("o botão de link para documento escreve `[[` e abre as sugestões", async () => {
    serve();
    const { area } = await mount();
    select(area, area.value.length);
    fireEvent.focus(area);
    fireEvent.click(screen.getByRole("button", { name: "Link para um documento da base" }));
    expect(area.value.endsWith("[[")).toBe(true);
    expect(screen.getByRole("listbox")).toBeTruthy();
  });
});

describe("rascunho local", () => {
  it("cada mudança fica no aparelho e volta ao abrir, com o aviso e o descarte", async () => {
    serve();
    const first = await mount();
    type(first.area, "rascunho");
    expect(readDraft(PID, API)).toMatchObject({ content: "rascunho", base: "sha-1" });
    cleanup();
    const { area } = await mount();
    expect(area.value).toBe("rascunho");
    expect(screen.getByText("Rascunho recuperado deste aparelho.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Descartar o rascunho" }));
    expect(area.value).toBe(ORIGINAL);
    expect(readDraft(PID, API)).toBeNull();
  });

  it("rascunho de uma versão antiga abre como conflito", async () => {
    const server = serve();
    writeDraft(PID, API, { title: "", content: "meu antigo", base: "sha-velha", at: 1 });
    server.sha = "sha-nova";
    const { area } = await mount();
    expect(area.value).toBe("meu antigo");
    expect(screen.getByRole("alert").textContent).toContain("Este documento mudou enquanto você escrevia");
    fireEvent.click(screen.getByRole("button", { name: "Recarregar" }));
    expect(area.value).toBe(ORIGINAL);
  });

  it("sem armazenamento, o editor segue sem rascunho", async () => {
    serve();
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("cota");
    });
    const { area } = await mount();
    type(area, "texto");
    expect(area.value).toBe("texto");
  });
});

describe("documento novo", () => {
  it("só é criado com o título confirmado; depois disso, o título fica fixo", async () => {
    const server = serve();
    await mount({ doc: null });
    expect(state()).toBe("Falta o título");
    const title = screen.getByRole("textbox", { name: "Título do documento" });
    fireEvent.change(title, { target: { value: "Check" } });
    await wait(1300);
    fireEvent.change(field(), { target: { value: "- item" } });
    await wait(1300);
    expect(server.posts).toEqual([]);
    expect(state()).toBe("Falta o título");
    fireEvent.change(title, { target: { value: "Checklist" } });
    fireEvent.keyDown(title, { key: "Enter" });
    await wait(1300);
    expect(server.posts).toEqual([{ title: "Checklist", content: "- item\n" }]);
    expect(state()).toBe("Salvo");
    expect(screen.queryByRole("textbox", { name: "Título do documento" })).toBeNull();
    expect(document.querySelector("h1.nd-kbe-title")?.textContent).toBe("Checklist");
  });

  it("o título confirmado vale também ao sair do campo; título vazio não confirma", async () => {
    const server = serve();
    await mount({ doc: null });
    const title = screen.getByRole("textbox", { name: "Título do documento" });
    fireEvent.change(title, { target: { value: "   " } });
    fireEvent.blur(title);
    fireEvent.change(field(), { target: { value: "x" } });
    await wait(1300);
    expect(server.posts).toEqual([]);
    fireEvent.change(title, { target: { value: "Guia" } });
    fireEvent.blur(title);
    await wait(1300);
    expect(server.posts.map((p) => p.title)).toEqual(["Guia"]);
  });
});
