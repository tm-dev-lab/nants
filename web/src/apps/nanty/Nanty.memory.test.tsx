/*
 * Aba Memória do Nanty (spec 140 T08), com a API simulada: as notas por tema com a procedência, a
 * marca de fora do índice, o runbook em bloco de código com o copiar, o texto nunca como HTML, a
 * edição com versão (e o conflito, com a escolha da pessoa), apagar e restaurar com o prazo, a cópia
 * baixada, os estados vazio, de erro e só leitura, e os textos em inglês.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiEvent } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { activateApp, renderShell, settle } from "../../desktop/shell-test-utils";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { makeProject, makeProjectState } from "../../test-utils";
import { parseMemoryView } from "./nantyMemoryApi";
import { memoryLayout, runbookParts } from "./nantyMemoryModel";

const P1 = makeProject("p1", "Loja");
const SPACE = { url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } };
const AT = Date.UTC(2026, 10, 2, 14, 30);
const DAY = 86_400_000;

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  setLanguage("pt-BR", { persist: false });
});

type Raw = Record<string, unknown>;

function note(id: string, topic: string, title: string, text: string, extra: Raw = {}, source: Raw = {}): Raw {
  return {
    id,
    topic,
    title,
    text,
    source: { kind: "person_said", channel: "telegram", at_ms: AT, ...source },
    created: AT,
    updated: AT,
    last_used: AT,
    state: "active",
    stale: false,
    ...extra,
  };
}

const ID_TONE = "a000000000000001";
const ID_DISK = "a000000000000002";
const ID_DEPLOY = "a000000000000003";
const ID_OLD = "a000000000000004";
const ID_GONE = "a000000000000005";

function memory(over: Raw = {}, entries: Raw[] = ENTRIES): Raw {
  const topics = ["identity", "working_together", "system", "runbooks", "decisions"].map((topic) => ({
    topic,
    status: "ok",
    entries: entries.filter((e) => e.topic === topic),
  }));
  return { writable: true, topics, limits: { title_max_chars: 80, fact_max_chars: 400, runbook_max_chars: 1500, max_active: 150, deleted_keep_days: 7 }, ...over };
}

const ENTRIES: Raw[] = [
  note(ID_TONE, "identity", "Tom", "Falar direto, com humor leve."),
  note(ID_DISK, "system", "Disco", "<img src=x onerror=alert(1)><b>negrito</b>", {}, { kind: "model_clean", channel: "web", project_id: "p1" }),
  note(ID_DEPLOY, "runbooks", "Deploy da loja", "Pare o app.\n```sh\ncargo build --release\n```\nDepois suba de novo.", {}, { kind: "person_confirmed" }),
  note(ID_OLD, "decisions", "Banco antigo", "Ficamos no SQLite.", { stale: true }),
  note(ID_GONE, "decisions", "Cor do botão", "Violeta.", { state: "deleted", deleted_at: AT, restore_until_ms: AT + 7 * DAY }),
];

async function open(body: Raw | (() => { status: number; body: unknown }) = memory()) {
  const view = await renderShell({ ...SPACE });
  view.server.reply("GET", /^\/api\/nanty\/memory$/, typeof body === "function" ? body : () => ({ status: 200, body }));
  await act(async () => activateApp("Nanty"));
  await settle();
  const win = document.querySelector<HTMLElement>('[data-window="nanty"]') as HTMLElement;
  await act(async () => {
    fireEvent.click(within(win).getByRole("tab", { name: /^(Memória|Memory)$/ }));
  });
  await settle();
  const tab = win.querySelector<HTMLElement>("[data-nanty-memory]") as HTMLElement;
  return { view, win, tab };
}

const entryEl = (tab: HTMLElement, id: string) => tab.querySelector<HTMLElement>(`[data-memory-entry="${id}"]`) as HTMLElement;

async function click(el: HTMLElement) {
  await act(async () => {
    fireEvent.click(el);
  });
  await settle();
}

describe("a lista", () => {
  it("por tema, com quem disse, onde e quando, a marca de fora do índice e as apagadas com o prazo", async () => {
    const { tab } = await open();
    const topics = [...tab.querySelectorAll("[data-memory-topic]")].map((s) => s.getAttribute("data-memory-topic"));
    expect(topics).toEqual(["identity", "system", "runbooks", "decisions", "deleted"]);
    expect(tab.querySelector('[data-memory-topic="working_together"]')).toBeNull();
    expect(within(tab).getByRole("heading", { name: "Identidade" })).toBeTruthy();
    expect(entryEl(tab, ID_TONE).querySelector("[data-memory-source]")?.textContent).toMatch(/^Você disse no Telegram, em .+/);
    expect(entryEl(tab, ID_DISK).querySelector("[data-memory-source]")?.textContent).toMatch(/^O Nanty guardou na janela, em .+ · Projeto Loja$/);
    expect(entryEl(tab, ID_DEPLOY).querySelector("[data-memory-source]")?.textContent).toMatch(/^Você confirmou num cartão no Telegram/);
    expect(entryEl(tab, ID_OLD).querySelector("[data-memory-stale]")?.textContent).toContain("saiu das notas que vão em toda conversa");
    expect(entryEl(tab, ID_TONE).querySelector("[data-memory-stale]")).toBeNull();
    const gone = tab.querySelector<HTMLElement>(`[data-memory-deleted="${ID_GONE}"]`) as HTMLElement;
    expect(gone.textContent).toContain("Decisões · Volta até");
    expect(within(gone).getByRole("button", { name: "Restaurar a nota Cor do botão" })).toBeTruthy();
    expect(tab.querySelector("[data-nanty-memory] .nd-nanty-lede")?.textContent).toContain("a apagada ainda volta por 7 dias");
    expect(findForbiddenGlyphs(tab)).toEqual([]);
  });

  it("o texto da nota é texto, nunca HTML", async () => {
    const { tab } = await open();
    const disk = entryEl(tab, ID_DISK);
    expect(disk.querySelector("img")).toBeNull();
    expect(disk.querySelector("b")).toBeNull();
    expect(disk.querySelector(".nd-nanty-memory-text")?.textContent).toBe("<img src=x onerror=alert(1)><b>negrito</b>");
  });

  it("o runbook mostra o comando exato em bloco de código, com o copiar", async () => {
    const writeText = vi.fn(() => Promise.resolve());
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    const { tab } = await open();
    const deploy = entryEl(tab, ID_DEPLOY);
    const code = deploy.querySelector<HTMLElement>("[data-memory-code]") as HTMLElement;
    expect(code.querySelector("pre code")?.textContent).toBe("cargo build --release");
    expect([...deploy.querySelectorAll(".nd-nanty-memory-text")].map((p) => p.textContent)).toEqual(["Pare o app.", "Depois suba de novo."]);
    await click(within(code).getByRole("button", { name: "Copiar o comando para Deploy da loja" }));
    expect(writeText).toHaveBeenCalledWith("cargo build --release");
    expect(within(code).getByRole("button").textContent).toContain("Copiado");
  });

  it("sem nota: o estado vazio; com erro: a mensagem traduzida e o tentar de novo", async () => {
    const empty = await open(memory({}, []));
    expect(empty.tab.querySelector('[data-nanty-empty="memory"]')?.textContent).toContain("Nada guardado ainda");
    cleanup();
    let fail = true;
    const { tab, view } = await open(() => (fail ? { status: 500, body: { error: "x", code: "nanty_memory_io", params: {} } } : { status: 200, body: memory() }));
    expect(tab.querySelector("[data-memory-error]")?.textContent).toContain("Não deu para ler a memória do Nanty: não deu para gravar a memória");
    fail = false;
    await click(within(tab).getByRole("button", { name: "Tentar de novo" }));
    expect(tab.querySelector("[data-memory-error]")).toBeNull();
    expect(entryEl(tab, ID_TONE)).toBeTruthy();
    expect(view.server.count("GET", /^\/api\/nanty\/memory$/)).toBeGreaterThanOrEqual(2);
  });

  it("outra instância e tema de versão mais nova: só leitura, sem botões de mudar", async () => {
    const ro = await open(memory({ writable: false }));
    expect(ro.tab.querySelector("[data-memory-readonly]")?.textContent).toContain("só para leitura");
    expect(within(ro.tab).queryByRole("button", { name: /^Editar a nota/ })).toBeNull();
    expect(within(ro.tab).queryByRole("button", { name: /^Restaurar a nota/ })).toBeNull();
    cleanup();
    const view = memory();
    (view.topics as Raw[])[2] = { topic: "system", status: "newer", entries: [ENTRIES[1]] };
    const { tab } = await open(view);
    expect(tab.querySelector('[data-memory-file="newer"]')?.textContent).toContain("versão mais nova do Nants");
    expect(within(entryEl(tab, ID_DISK)).queryByRole("button", { name: /Editar/ })).toBeNull();
    expect(within(entryEl(tab, ID_TONE)).getByRole("button", { name: "Editar a nota Tom" })).toBeTruthy();
  });
});

describe("mudar", () => {
  async function startEdit(tab: HTMLElement, id: string, title: string) {
    await click(within(entryEl(tab, id)).getByRole("button", { name: `Editar a nota ${title}` }));
    return tab.querySelector<HTMLElement>(`[data-memory-editor="${id}"]`) as HTMLElement;
  }

  it("editar grava sobre a versão lida e relê a lista", async () => {
    const { view, tab } = await open();
    const sent: unknown[] = [];
    view.server.reply("PUT", /^\/api\/nanty\/memory\/a000000000000001$/, (body) => {
      sent.push(body);
      return { status: 200, body: { entry: { ...ENTRIES[0], text: "Direto e curto.", updated: AT + 1, source: { kind: "person_said", channel: "web", at_ms: AT + 1 } } } };
    });
    const editor = await startEdit(tab, ID_TONE, "Tom");
    expect(editor.textContent).toContain("29 de 400");
    fireEvent.change(within(editor).getByLabelText("Texto"), { target: { value: "  Direto e curto.  " } });
    await click(within(editor).getByRole("button", { name: "Salvar" }));
    expect(sent).toEqual([{ title: "Tom", text: "Direto e curto.", base_updated: AT }]);
    expect(tab.querySelector("[data-nanty-notice]")?.textContent).toBe("Nota gravada: Tom.");
    expect(tab.querySelector("[data-memory-editor]")).toBeNull();
  });

  it("texto acima do teto trava o salvar; o erro do servidor sai traduzido", async () => {
    const { view, tab } = await open();
    view.server.reply("PUT", /^\/api\/nanty\/memory\//, () => ({ status: 422, body: { error: "x", code: "nanty_memory_empty", params: {} } }));
    const editor = await startEdit(tab, ID_TONE, "Tom");
    const save = within(editor).getByRole("button", { name: "Salvar" }) as HTMLButtonElement;
    fireEvent.change(within(editor).getByLabelText("Texto"), { target: { value: "x".repeat(401) } });
    expect(save.disabled).toBe(true);
    fireEvent.change(within(editor).getByLabelText("Texto"), { target: { value: "ok" } });
    await click(save);
    expect(tab.querySelector("[data-nanty-notice]")?.textContent).toBe("Nada mudou: a nota da memória precisa de título e texto");
  });

  it("conflito: mostra a versão gravada agora e a pessoa escolhe qual fica", async () => {
    const { view, tab } = await open();
    const theirs = { ...ENTRIES[0], text: "Versão do Nanty, gravada no meio.", updated: AT + 5, source: { kind: "model_clean", channel: "telegram", at_ms: AT + 5 } };
    const sent: Raw[] = [];
    view.server.reply("PUT", /^\/api\/nanty\/memory\/a000000000000001$/, (body) => {
      sent.push(body as Raw);
      return sent.length === 1
        ? { status: 409, body: { error: "x", code: "nanty_memory_conflict", params: {}, entry: theirs } }
        : { status: 200, body: { entry: { ...theirs, text: "Minha versão", updated: AT + 6 } } };
    });
    const editor = await startEdit(tab, ID_TONE, "Tom");
    view.server.reply("GET", /^\/api\/nanty\/memory$/, () => ({ status: 200, body: memory({}, [theirs, ...ENTRIES.slice(1)]) }));
    fireEvent.change(within(editor).getByLabelText("Texto"), { target: { value: "Minha versão" } });
    await click(within(editor).getByRole("button", { name: "Salvar" }));
    const conflict = tab.querySelector<HTMLElement>("[data-memory-conflict]") as HTMLElement;
    expect(conflict.textContent).toContain("Esta nota mudou enquanto você editava");
    expect(conflict.querySelector("[data-memory-current]")?.textContent).toContain("Versão do Nanty, gravada no meio.");
    expect(conflict.querySelector("[data-memory-source]")?.textContent).toMatch(/^O Nanty guardou no Telegram/);
    expect((within(editor).getByLabelText("Texto") as HTMLTextAreaElement).value).toBe("Minha versão");
    await click(within(conflict).getByRole("button", { name: "Gravar a minha por cima" }));
    expect(sent.map((b) => b.base_updated)).toEqual([AT, AT + 5]);
    expect(tab.querySelector("[data-nanty-notice]")?.textContent).toBe("Nota gravada: Tom.");
  });

  it("conflito com a releitura falhando: mostra o erro, mantém o rascunho e nunca diz que foi apagada", async () => {
    const { view, tab } = await open();
    view.server.reply("PUT", /^\/api\/nanty\/memory\//, () => ({ status: 409, body: { error: "x", code: "nanty_memory_conflict", params: {} } }));
    const editor = await startEdit(tab, ID_TONE, "Tom");
    view.server.reply("GET", /^\/api\/nanty\/memory$/, () => ({ status: 500, body: { error: "x", code: "nanty_memory_io", params: {} } }));
    fireEvent.change(within(editor).getByLabelText("Texto"), { target: { value: "Minha versão" } });
    await click(within(editor).getByRole("button", { name: "Salvar" }));
    expect(tab.querySelector("[data-memory-gone]")).toBeNull();
    expect(tab.querySelector("[data-nanty-notice]")?.textContent).toBe(
      "A nota mudou enquanto você editava, mas não deu para ler a versão gravada: não deu para gravar a memória. O seu texto continua aqui; tente salvar de novo.",
    );
    const still = tab.querySelector<HTMLElement>(`[data-memory-editor="${ID_TONE}"]`) as HTMLElement;
    expect((within(still).getByLabelText("Texto") as HTMLTextAreaElement).value).toBe("Minha versão");
    expect((within(still).getByRole("button", { name: "Salvar" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("conflito com a releitura do aviso da conversa ao mesmo tempo: mostra a versão gravada, não 'apagada'", async () => {
    const { view, tab } = await open();
    const theirs = { ...ENTRIES[0], text: "Gravada pelo Nanty no meio.", updated: AT + 5, source: { kind: "model_clean", channel: "web", at_ms: AT + 5 } };
    const editor = await startEdit(tab, ID_TONE, "Tom");
    view.server.reply("GET", /^\/api\/nanty\/memory$/, () => ({ status: 200, body: memory({}, [theirs, ...ENTRIES.slice(1)]) }));
    view.server.reply("GET", /^\/api\/nanty\/conversation$/, () => ({
      status: 200,
      body: { messages: [{ id: "m1", at_ms: AT + 5, from: "nanty", channel: "web", project_id: null, text: "Guardei.", code: null, params: {} }] },
    }));
    view.server.reply("PUT", /^\/api\/nanty\/memory\//, () => {
      // O Nanty gravou a nota e a conversa mudou: a aba relê a memória junto com o conflito.
      view.stream().emit("nanty_changed", { event_type: "nanty_changed", project_id: null, payload: { part: "conversation" } } as unknown as ApiEvent);
      return { status: 409, body: { error: "x", code: "nanty_memory_conflict", params: {} } };
    });
    await click(within(editor).getByRole("button", { name: "Salvar" }));
    await settle();
    expect(tab.querySelector("[data-memory-gone]")).toBeNull();
    expect(tab.querySelector("[data-memory-conflict] [data-memory-current]")?.textContent).toContain("Gravada pelo Nanty no meio.");
  });

  it("conflito: ficar com a gravada fecha a edição sem gravar", async () => {
    const { view, tab } = await open();
    const theirs = { ...ENTRIES[0], text: "A gravada.", updated: AT + 5 };
    view.server.reply("PUT", /^\/api\/nanty\/memory\//, () => ({ status: 409, body: { error: "x", code: "nanty_memory_conflict", params: {} } }));
    const editor = await startEdit(tab, ID_TONE, "Tom");
    view.server.reply("GET", /^\/api\/nanty\/memory$/, () => ({ status: 200, body: memory({}, [theirs, ...ENTRIES.slice(1)]) }));
    await click(within(editor).getByRole("button", { name: "Salvar" }));
    await click(within(tab).getByRole("button", { name: "Ficar com a gravada" }));
    expect(tab.querySelector("[data-memory-editor]")).toBeNull();
    expect(entryEl(tab, ID_TONE).textContent).toContain("A gravada.");
    expect(view.server.count("PUT", /^\/api\/nanty\/memory\//)).toBe(1);
  });

  it("apagar diz o prazo, e restaurar traz de volta", async () => {
    const { view, tab } = await open();
    view.server.reply("POST", /^\/api\/nanty\/memory\/a000000000000001\/delete$/, () => ({ status: 200, body: { entry: { ...ENTRIES[0], state: "deleted", deleted_at: AT } } }));
    view.server.reply("GET", /^\/api\/nanty\/memory$/, () => ({
      status: 200,
      body: memory({}, [{ ...ENTRIES[0], state: "deleted", deleted_at: AT, restore_until_ms: AT + 7 * DAY }, ...ENTRIES.slice(1)]),
    }));
    await click(within(entryEl(tab, ID_TONE)).getByRole("button", { name: "Apagar a nota Tom" }));
    expect(tab.querySelector("[data-nanty-notice]")?.textContent).toBe("Apagada: Tom. Dá para restaurar por 7 dias.");
    expect(entryEl(tab, ID_TONE)).toBeNull();
    const deleted = tab.querySelector<HTMLElement>(`[data-memory-deleted="${ID_TONE}"]`) as HTMLElement;
    expect(deleted.textContent).toContain("Identidade · Volta até");

    view.server.reply("POST", /^\/api\/nanty\/memory\/a000000000000001\/restore$/, () => ({ status: 200, body: { entry: ENTRIES[0] } }));
    view.server.reply("GET", /^\/api\/nanty\/memory$/, () => ({ status: 200, body: memory() }));
    await click(within(deleted).getByRole("button", { name: "Restaurar a nota Tom" }));
    expect(tab.querySelector("[data-nanty-notice]")?.textContent).toBe("De volta à memória: Tom.");
    expect(entryEl(tab, ID_TONE)).toBeTruthy();

    view.server.reply("POST", /\/restore$/, () => ({ status: 404, body: { error: "x", code: "nanty_memory_gone", params: {} } }));
    await click(within(tab.querySelector<HTMLElement>(`[data-memory-deleted="${ID_GONE}"]`) as HTMLElement).getByRole("button", { name: /Restaurar/ }));
    expect(tab.querySelector("[data-nanty-notice]")?.textContent).toBe("Nada mudou: a nota apagada já saiu da memória");
  });

  it("baixar a cópia: pede ao servidor e grava o arquivo; o erro sai traduzido", async () => {
    const { view, tab } = await open();
    const exported = { version: 1, exported_at_ms: AT, topics: { identity: [ENTRIES[0]] } };
    view.server.reply("POST", /^\/api\/nanty\/memory\/export$/, () => ({ status: 200, body: exported }));
    const blobs: Blob[] = [];
    const create = vi.fn((b: Blob) => {
      blobs.push(b);
      return "blob:memoria";
    });
    vi.stubGlobal("URL", Object.assign(URL, { createObjectURL: create, revokeObjectURL: vi.fn() }));
    const clicked = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
      expect(this.download).toMatch(/^nanty-memory-\d{4}-\d{2}-\d{2}\.json$/);
    });
    await click(within(tab).getByRole("button", { name: "Baixar uma cópia" }));
    expect(view.server.count("POST", /^\/api\/nanty\/memory\/export$/)).toBe(1);
    expect(clicked).toHaveBeenCalledTimes(1);
    expect(blobs).toHaveLength(1);
    expect(JSON.parse(await (blobs[0] as Blob).text())).toEqual(exported);
    expect(tab.querySelector("[data-nanty-notice]")?.textContent).toContain("Cópia baixada");

    view.server.reply("POST", /^\/api\/nanty\/memory\/export$/, () => ({ status: 401, body: { error: "x", code: "device_required", params: {} } }));
    await click(within(tab).getByRole("button", { name: "Baixar uma cópia" }));
    expect(tab.querySelector("[data-nanty-notice]")?.textContent).toMatch(/^A cópia não saiu: /);
    expect(tab.querySelector("[data-nanty-notice]")?.getAttribute("data-tone")).toBe("warn");
  });
});

describe("em inglês", () => {
  it("os textos vêm do dicionário, sem sobra em português", async () => {
    setLanguage("en", { persist: false });
    const { tab } = await open();
    expect(within(tab).getByRole("heading", { name: "What Nanty keeps from one conversation to the next" })).toBeTruthy();
    expect(entryEl(tab, ID_TONE).querySelector("[data-memory-source]")?.textContent).toMatch(/^You said it on Telegram, on /);
    expect(screen.getByRole("button", { name: "Download a copy" })).toBeTruthy();
    const own = /^(Loja|Tom|Disco|Deploy da loja|Banco antigo|Cor do botão|Falar direto, com humor leve\.|Pare o app\.|Depois suba de novo\.|Ficamos no SQLite\.|Violeta\.|cargo build --release|<img src=x onerror=alert\(1\)><b>negrito<\/b>)$/;
    const titles = ["Tom", "Disco", "Deploy da loja", "Banco antigo", "Cor do botão", "Loja"];
    expect(portugueseLeftovers(tab).filter((text) => !own.test(text) && !titles.some((title) => text.endsWith(title) || text.includes(` ${title}`)))).toEqual([]);
  });
});

describe("o modelo", () => {
  it("no tema, as notas em uso primeiro e as fora do índice no fim", () => {
    const view = parseMemoryView(
      memory({}, [
        note("b000000000000001", "system", "Antiga e esquecida", "x", { stale: true, created: AT - 10 }),
        note("b000000000000002", "system", "Em uso", "y", { created: AT }),
        note("b000000000000003", "system", "Também esquecida", "z", { stale: true, created: AT - 5 }),
      ]),
    );
    const ids = memoryLayout(view as NonNullable<typeof view>).sections[0]?.entries.map((e) => e.id);
    expect(ids).toEqual(["b000000000000002", "b000000000000001", "b000000000000003"]);
  });

  it("runbook guardado pelo Nanty pede para conferir antes de rodar; o seu, não", async () => {
    const mine = note("c000000000000001", "runbooks", "Meu passo", "npm run lint", {}, { kind: "person_said" });
    const his = note("c000000000000002", "runbooks", "Passo dele", "npm run deploy", {}, { kind: "model_clean" });
    const { tab } = await open(memory({}, [mine, his, ENTRIES[2] as Raw]));
    expect(entryEl(tab, "c000000000000002").querySelector("[data-memory-check]")?.textContent).toBe("O Nanty guardou este comando sozinho. Confira antes de rodar.");
    expect(entryEl(tab, "c000000000000001").querySelector("[data-memory-check]")).toBeNull();
    expect(entryEl(tab, ID_DEPLOY).querySelector("[data-memory-check]")).toBeNull();
    cleanup();
    setLanguage("en", { persist: false });
    const en = await open(memory({}, [his]));
    expect(entryEl(en.tab, "c000000000000002").querySelector("[data-memory-check]")?.textContent).toBe("Nanty saved this command on its own. Check it before you run it.");
  });
  it("o runbook sem marcação é todo comando; com crases simples, cada trecho vira código", () => {
    expect(runbookParts("docker compose up -d")).toEqual([{ kind: "code", text: "docker compose up -d" }]);
    expect(runbookParts("Rode `make test` e depois `make lint`.")).toEqual([
      { kind: "text", text: "Rode" },
      { kind: "code", text: "make test" },
      { kind: "text", text: "e depois" },
      { kind: "code", text: "make lint" },
      { kind: "text", text: "." },
    ]);
  });

  it("descarta entrada que não confere e põe as apagadas à parte", () => {
    const view = parseMemoryView(memory({}, [...ENTRIES, { id: "x", topic: "system", title: "t", text: "t" }]));
    expect(view).not.toBeNull();
    const layout = memoryLayout(view as NonNullable<typeof view>);
    expect(layout.sections.flatMap((s) => s.entries.map((e) => e.id))).toEqual([ID_TONE, ID_DISK, ID_DEPLOY, ID_OLD]);
    expect(layout.deleted.map((e) => e.id)).toEqual([ID_GONE]);
    expect(parseMemoryView({ topics: [] })).toBeNull();
  });
});

