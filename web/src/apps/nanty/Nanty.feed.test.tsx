/*
 * Feed do Nanty na janela (spec 138 T10, critérios 1, 3 e 6): as seções, o porquê, os botões do
 * Telegram, as decisões (feita, vencida e com erro), a atualização pelo SSE, o desfazer, o previsto
 * e a troca entre o feed e a conversa na janela estreita.
 */
import { act, cleanup, fireEvent, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { ApiEvent } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { activateApp, NANTY_EMPTY_FEED, NANTY_STATUS, renderShell, settle } from "../../desktop/shell-test-utils";
import { fakeResizeObservers } from "../../desktop/test-utils";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { makeProject, makeProjectState } from "../../test-utils";

const P1 = makeProject("p1", "Loja");
const SPACE = { url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } };
const NOW = Date.now();

const PLAN_CARD = {
  id: "aaaaaaaaaaaaaaaa",
  project_id: "p1",
  group: "p1:plan_waiting",
  code: "nanty.why.plan_waiting",
  params: { spec_id: "s1", since: "14:02", observer: "attention", age_min: 180 },
  action: "approve_plan",
  token: "tok-1",
  created_at_ms: NOW - 3 * 3600_000,
  expires_at_ms: NOW + 3600_000,
};

const DISK_CARD = {
  id: "bbbbbbbbbbbbbbbb",
  group: "*:disk_low",
  code: "nanty.why.disk_low",
  params: { min_gb: 20, count: 1, free_gb: 8, since: "07:42" },
  token: "tok-2",
  created_at_ms: NOW - 60_000,
  expires_at_ms: NOW + 3600_000,
};

const UNDO = { id: "cccccccccccccccc", token: "undo-tok", expires_at_ms: NOW + 3600_000 };

const FEED = {
  ...NANTY_EMPTY_FEED,
  waiting: [PLAN_CARD, DISK_CARD],
  done: [
    { at_ms: NOW - 120_000, project_id: "p1", code: "nanty.action.swap_reserve", params: { role: "developer" }, action: "swap_reserve", decided_by: "auto", result: "ok", undo: UNDO },
    { at_ms: NOW - 60_000, code: "nanty.why.tmp_low", params: {}, result: "resolved" },
  ],
  planned: [{ project_id: "p1", action: "retry_provider", code: "nanty.action.retry_provider", params: { attempt: 2 }, at_ms: NOW + 600_000 }],
};

afterEach(() => {
  cleanup();
  setLanguage("pt-BR", { persist: false });
});

function nantyWindow(): HTMLElement {
  const win = document.querySelector<HTMLElement>('[data-window="nanty"]');
  if (win === null) throw new Error("janela do Nanty ausente");
  return win;
}

async function open(feed: unknown = FEED, extra: Record<string, unknown> = {}) {
  const view = await renderShell({ ...SPACE, nanty: { status: { ...NANTY_STATUS, avatar: "waiting" }, feed }, ...extra });
  view.server.reply("GET", /^\/api\/nanty\/config$/, () => ({ status: 200, body: { digest_time: "08:00" } }));
  await act(async () => activateApp("Nanty"));
  await settle();
  return { view, win: nantyWindow() };
}

const section = (win: HTMLElement, id: string): HTMLElement => {
  const el = win.querySelector<HTMLElement>(`[data-nanty-section="${id}"]`);
  if (el === null) throw new Error(`seção ${id} ausente`);
  return el;
};

describe("feed do Nanty", () => {
  it("as quatro seções, com o título, os fatos, o porquê e os botões do Telegram", async () => {
    const { win } = await open();
    const waiting = section(win, "waiting");
    expect(waiting.querySelector("h3")?.textContent).toBe("Esperando você · 2");
    const plan = waiting.querySelector<HTMLElement>('[data-nanty-card="aaaaaaaaaaaaaaaa"]') as HTMLElement;
    expect(plan.querySelector("h4")?.textContent).toBe("Plano esperando a sua aprovação");
    // C-18: só contagens e códigos (o projeto pelo nome cadastrado, a spec e a idade).
    expect(plan.querySelector("[data-nanty-facts]")?.textContent).toBe("Loja · Spec s1 · há 3 h");
    expect(plan.querySelector("[data-nanty-why]")?.textContent).toBe("Observador de atenção: um plano espera a sua aprovação desde 14:02.");
    const buttons = within(plan).getAllByRole("button").map((b) => b.textContent);
    expect(buttons).toEqual(["Aprovar o plano", "Não", "Lembrar depois"]);
    // Só a ação do primeiro cartão é a primária da janela.
    expect(win.querySelectorAll('[data-window-content] [data-variant="primary"]')).toHaveLength(1);
    const disk = waiting.querySelector<HTMLElement>('[data-nanty-card="bbbbbbbbbbbbbbbb"]') as HTMLElement;
    expect(disk.querySelector("[data-nanty-facts]")?.textContent).toBe("8 GB livres");
    expect(within(disk).getAllByRole("button").map((b) => b.textContent)).toEqual(["Lembrar depois"]);

    const auto = section(win, "auto");
    expect(auto.querySelector("h3")?.textContent).toBe("Feito por conta própria · 1");
    expect(auto.textContent).toContain("Trocar para a reserva e seguir");
    expect(auto.textContent).toContain("Feito, pelo Nanty, sozinho");
    expect(within(auto).getByRole("button", { name: "Desfazer" })).toBeTruthy();

    const planned = section(win, "planned");
    expect(planned.querySelector("h3")?.textContent).toBe("Previsto · 2");
    expect(planned.querySelector('[data-nanty-planned="retry_provider"]')?.textContent).toContain("Tentativa 2");
    expect(planned.querySelector('[data-nanty-planned="digest"]')?.textContent).toContain("Resumo da manhã às 08:00");

    const today = section(win, "today");
    expect(today.textContent).toContain("Pouco espaço na pasta temporária · Resolvido");
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("decidir aqui: o POST leva o token e a resposta; o resultado aparece e o feed se relê", async () => {
    const { view, win } = await open();
    const posted: unknown[] = [];
    view.server.reply("POST", /^\/api\/nanty\/cards\/aaaaaaaaaaaaaaaa\/decide$/, (body) => {
      posted.push(body);
      return { status: 200, body: { outcome: "done", code: "nanty.outcome.done", params: {} } };
    });
    const feedReads = view.server.count("GET", /^\/api\/nanty\/feed$/);
    await act(async () => {
      fireEvent.click(within(win).getByRole("button", { name: "Aprovar o plano" }));
    });
    await settle();
    expect(posted).toEqual([{ token: "tok-1", answer: "yes" }]);
    expect(win.querySelector("[data-nanty-notice]")?.textContent).toBe("Feito.");
    expect(win.querySelector("[data-nanty-notice]")?.getAttribute("data-tone")).toBe("ok");
    expect(view.server.count("GET", /^\/api\/nanty\/feed$/)).toBeGreaterThan(feedReads);
  });

  it("cartão vencido e token trocado: o texto do código, sem quebrar a janela", async () => {
    const { view, win } = await open();
    view.server.reply("POST", /\/decide$/, () => ({ status: 200, body: { outcome: "expired", code: "nanty.outcome.expired", params: {} } }));
    await act(async () => {
      fireEvent.click(within(win).getAllByRole("button", { name: "Não" })[0] as HTMLElement);
    });
    await settle();
    expect(win.querySelector("[data-nanty-notice]")?.textContent).toBe("Este cartão expirou: a situação mudou desde o aviso.");
    expect(win.querySelector("[data-nanty-notice]")?.getAttribute("data-tone")).toBe("warn");

    view.server.reply("POST", /\/decide$/, () => ({ status: 404, body: { error: "não encontrado", code: "not_found", params: {} } }));
    await act(async () => {
      fireEvent.click(within(win).getAllByRole("button", { name: "Lembrar depois" })[1] as HTMLElement);
    });
    await settle();
    expect(win.querySelector("[data-nanty-notice]")?.textContent).toBe("não encontrado");
    // A janela continua inteira: as seções e os botões seguem lá.
    expect(section(win, "waiting").querySelectorAll("[data-nanty-card]")).toHaveLength(2);
  });

  it("decidido no Telegram: o nanty_changed relê o feed e o cartão sai sozinho", async () => {
    const { view, win } = await open();
    expect(win.querySelector('[data-nanty-card="aaaaaaaaaaaaaaaa"]')).not.toBeNull();
    view.server.reply("GET", /^\/api\/nanty\/feed$/, () => ({
      status: 200,
      body: { ...FEED, waiting: [DISK_CARD], done: [...FEED.done, { at_ms: NOW, project_id: "p1", code: "nanty.action.approve_plan", params: {}, action: "approve_plan", decided_by: "telegram", result: "ok" }] },
    }));
    await act(async () => {
      view.stream().emit("nanty_changed", { event_type: "nanty_changed", project_id: null, payload: { part: "feed" } } as unknown as ApiEvent);
    });
    await settle();
    expect(win.querySelector('[data-nanty-card="aaaaaaaaaaaaaaaa"]')).toBeNull();
    expect(section(win, "waiting").querySelector("h3")?.textContent).toBe("Esperando você · 1");
    expect(section(win, "today").textContent).toContain("Aprovar o plano · Loja · Feito, por você, no Telegram");
  });

  it("desfazer: o POST com o token do desfazer e o resultado pelo dicionário", async () => {
    const { view, win } = await open();
    const posted: unknown[] = [];
    view.server.reply("POST", /^\/api\/nanty\/undo\/cccccccccccccccc$/, (body) => {
      posted.push(body);
      return { status: 200, body: { outcome: "undone", code: "nanty.outcome.undone", params: {} } };
    });
    await act(async () => {
      fireEvent.click(within(section(win, "auto")).getByRole("button", { name: "Desfazer" }));
    });
    await settle();
    expect(posted).toEqual([{ token: "undo-tok" }]);
    expect(win.querySelector("[data-nanty-notice]")?.textContent).toBe("Desfeito: a conexão anterior voltou.");
  });

  it("feito com falha mostra o erro traduzido; o desfazer vencido some", async () => {
    const failed = { at_ms: NOW - 1000, code: "nanty.action.retry_provider", params: { error: "nanty_unavailable" }, action: "retry_provider", decided_by: "auto", result: "failed" };
    const expired = { ...FEED.done[0], undo: { ...UNDO, expires_at_ms: NOW - 1 } };
    const { win } = await open({ ...NANTY_EMPTY_FEED, done: [failed, expired] });
    const auto = section(win, "auto");
    expect(auto.textContent).toContain("Não deu certo, pelo Nanty, sozinho. Erro: o Nanty está ativo em outra instância do Nants");
    expect(within(auto).queryByRole("button", { name: "Desfazer" })).toBeNull();
  });

  it("sem nada esperando: o estado calmo com o avatar; o resumo da manhã segue em Previsto", async () => {
    const { win } = await open(NANTY_EMPTY_FEED);
    expect(win.querySelector('[data-nanty-empty="feed"]')?.textContent).toContain("Tudo calmo por aqui");
    expect(section(win, "planned").textContent).toContain("Resumo da manhã às 08:00");
    // "Mudar horário" leva aos Ajustes.
    fireEvent.click(within(win).getByRole("button", { name: "Mudar horário" }));
    expect(within(win).getByRole("tab", { name: "Ajustes" }).getAttribute("aria-selected")).toBe("true");
  });

  it("em en, sem português", async () => {
    setLanguage("en", { persist: false });
    const { win } = await open();
    expect(section(win, "waiting").querySelector("h3")?.textContent).toBe("Waiting for you · 2");
    expect(win.querySelector('[data-nanty-card="aaaaaaaaaaaaaaaa"] h4')?.textContent).toBe("Plan waiting for your approval");
    expect(portugueseLeftovers(win.querySelector("[data-window-content]") as HTMLElement)).toEqual([]);
  });

  it("janela larga: o feed e a conversa lado a lado; estreita: um seletor alterna os dois", async () => {
    const observers = fakeResizeObservers();
    const { win } = await open(FEED, { createObserver: observers.factory });
    await act(async () => observers.resize(1100));
    expect(win.querySelector('[data-nanty-split="true"], .nd-nanty-split[data-split="true"]')).not.toBeNull();
    expect(win.querySelector("[data-nanty-feed]")).not.toBeNull();
    expect(win.querySelector("[data-nanty-chat]")).not.toBeNull();
    expect(win.querySelector("[data-nanty-switch]")).toBeNull();
    await act(async () => observers.resize(380));
    const sw = win.querySelector<HTMLElement>("[data-nanty-switch]") as HTMLElement;
    expect(within(sw).getAllByRole("button").map((b) => b.textContent)).toEqual(["Feed", "Conversa"]);
    expect(win.querySelector("[data-nanty-chat]")).toBeNull();
    fireEvent.click(within(sw).getByRole("button", { name: "Conversa" }));
    expect(win.querySelector("[data-nanty-chat]")).not.toBeNull();
    expect(win.querySelector("[data-nanty-feed]")).toBeNull();
  });
});
