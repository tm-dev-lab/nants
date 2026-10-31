/*
 * Conversa do Nanty na janela (spec 138 T10, critério 2): a mesma do Telegram, com a origem; o texto
 * fixo pelo dicionário; as notas de voz tocáveis pela rota da T09, com a transcrição (e o aviso com o
 * baixar quando o navegador não toca Ogg/Opus); o envio pelo POST; o gravar desligado com a dica; o
 * SSE da conversa; e os botões do cartão citado na mensagem.
 */
import { act, cleanup, fireEvent, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiEvent } from "../../api/types";
import { activateApp, NANTY_EMPTY_FEED, NANTY_STATUS, renderShell, settle } from "../../desktop/shell-test-utils";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { makeProject, makeProjectState } from "../../test-utils";

const P1 = makeProject("p1", "Loja");
const SPACE = { url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } };
const AT = new Date(2026, 9, 30, 14, 20).getTime();
const AUDIO = { id: "0123456789abcdef", duration_secs: 21, bytes: 42_000 };

const CARD = {
  id: "aaaaaaaaaaaaaaaa",
  project_id: "p1",
  group: "p1:pause",
  code: "nanty.card.pause_run",
  params: { spec_id: "s1" },
  action: "pause_run",
  token: "tok-p",
  created_at_ms: AT,
  expires_at_ms: Date.now() + 3600_000,
};

const MESSAGES = [
  { id: "m1", at_ms: AT, from: "person", channel: "telegram", text: "olha a Loja pra mim", voice: true, audio: AUDIO },
  { id: "m2", at_ms: AT + 1000, from: "nanty", channel: "telegram", text: "A Loja está em andamento.", lang: "pt-BR", audio: { ...AUDIO, id: "fedcba9876543210" } },
  { id: "m3", at_ms: AT + 2000, from: "person", channel: "web", text: "/status" },
  {
    id: "m4",
    at_ms: AT + 3000,
    from: "nanty",
    channel: "web",
    code: "nanty.chat.status",
    params: { waiting: 1, done: 0, planned: 2, lines: [{ code: "nanty.chat.status_budget_usd", params: { spent: 0.06, budget: 0.5 } }] },
  },
  { id: "m5", at_ms: AT + 4000, from: "nanty", channel: "web", code: "nanty.chat.pause_card", params: { project: "Loja" }, card_ids: ["aaaaaaaaaaaaaaaa"] },
];

let restore: (() => void) | null = null;

afterEach(() => {
  cleanup();
  restore?.();
  restore = null;
  setLanguage("pt-BR", { persist: false });
});

/** O navegador da vez toca (ou não) Opus em OGG. */
function playsOgg(yes: boolean): void {
  const spy = vi.spyOn(HTMLMediaElement.prototype, "canPlayType").mockImplementation((type: string) => (yes && type.includes("ogg") ? "probably" : ""));
  restore = () => spy.mockRestore();
}

function chat(): HTMLElement {
  const el = document.querySelector<HTMLElement>('[data-window="nanty"] [data-nanty-chat]');
  if (el === null) throw new Error("conversa ausente");
  return el;
}

async function open(messages: unknown[] = MESSAGES, feed: unknown = { ...NANTY_EMPTY_FEED, waiting: [CARD] }) {
  const view = await renderShell({ ...SPACE, nanty: { status: NANTY_STATUS, feed, conversation: { messages } } });
  await act(async () => activateApp("Nanty"));
  await settle();
  return view;
}

const msg = (id: string): HTMLElement => {
  const el = chat().querySelector<HTMLElement>(`[data-nanty-msg="${id}"]`);
  if (el === null) throw new Error(`mensagem ${id} ausente`);
  return el;
};

describe("conversa do Nanty", () => {
  it("a mesma do Telegram, com a origem de cada mensagem e o texto fixo pelo dicionário", async () => {
    playsOgg(true);
    await open();
    const head = chat().querySelector(".nd-nanty-chat-head");
    expect(head?.textContent).toBe("Conversaa mesma do Telegram");
    expect(msg("m1").querySelector("[data-nanty-origin]")?.textContent).toBe("você, pelo Telegram · nota de voz · 14:20");
    expect(msg("m2").querySelector("[data-nanty-origin]")?.textContent).toBe("Nanty, pelo Telegram · voz e texto · 14:20");
    expect(msg("m3").querySelector("[data-nanty-origin]")?.textContent).toBe("você, pela janela · 14:20");
    expect(msg("m4").querySelector(".nd-nanty-bubble")?.textContent).toBe(
      "O Nanty está atento. Esperando você: 1. Feito hoje: 0. Previsto: 2.Gasto do modelo hoje: US$ 0,06 de US$ 0,50.",
    );
    // A transcrição da nota de voz da pessoa é o texto dela.
    expect(msg("m1").querySelector(".nd-nanty-bubble")?.textContent).toBe("olha a Loja pra mim");
  });

  it("nota de voz tocável pela rota da T09: o áudio com Range é do servidor; aqui, tocar e pausar", async () => {
    playsOgg(true);
    const play = vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(function (this: HTMLMediaElement) {
      this.dispatchEvent(new Event("play"));
      return Promise.resolve();
    });
    const pause = vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(function (this: HTMLMediaElement) {
      this.dispatchEvent(new Event("pause"));
    });
    try {
      await open();
      const voice = msg("m2").querySelector<HTMLElement>('[data-nanty-voice="player"]') as HTMLElement;
      expect(voice.querySelector("audio")?.getAttribute("src")).toBe("/api/nanty/audio/fedcba9876543210");
      expect(voice.querySelector("audio")?.getAttribute("preload")).toBe("none");
      expect(voice.textContent).toContain("0:21");
      await act(async () => {
        fireEvent.click(within(voice).getByRole("button", { name: "Ouvir a nota de voz" }));
      });
      expect(play).toHaveBeenCalledTimes(1);
      await act(async () => {
        fireEvent.click(within(voice).getByRole("button", { name: "Pausar a nota de voz" }));
      });
      expect(pause).toHaveBeenCalledTimes(1);
      expect(within(voice).getByRole("button", { name: "Ouvir a nota de voz" })).toBeTruthy();
    } finally {
      play.mockRestore();
      pause.mockRestore();
    }
  });

  it("navegador sem Ogg/Opus: a transcrição fica, com um aviso discreto e o baixar", async () => {
    playsOgg(false);
    await open();
    const voice = msg("m1").querySelector<HTMLElement>('[data-nanty-voice="unsupported"]') as HTMLElement;
    expect(voice.textContent).toContain("Este navegador não toca este áudio.");
    const link = within(voice).getByRole("link", { name: "Baixar o áudio" });
    expect(link.getAttribute("href")).toBe("/api/nanty/audio/0123456789abcdef");
    expect(link.hasAttribute("download")).toBe(true);
    expect(msg("m1").querySelector(".nd-nanty-bubble")?.textContent).toBe("olha a Loja pra mim");
    expect(msg("m1").querySelector("audio")).toBeNull();
  });

  it("nota de voz sem transcrição diz isso", async () => {
    playsOgg(true);
    await open([{ id: "v1", at_ms: AT, from: "person", channel: "telegram", voice: true, audio: AUDIO }]);
    expect(msg("v1").querySelector(".nd-nanty-bubble")?.textContent).toBe("Nota de voz sem transcrição.");
  });

  it("enviar: o POST com o texto, o campo limpa e a conversa se relê; o SSE traz a resposta", async () => {
    const view = await open([]);
    expect(chat().querySelector("[data-nanty-chat-empty]")?.textContent).toContain("/resumo");
    const posted: unknown[] = [];
    view.server.reply("POST", /^\/api\/nanty\/conversation$/, (body) => {
      posted.push(body);
      return { status: 202, body: { id: "x1" } };
    });
    const input = within(chat()).getByRole("textbox", { name: "Mensagem para o Nanty" }) as HTMLInputElement;
    const send = within(chat()).getByRole("button", { name: "Enviar" }) as HTMLButtonElement;
    expect(send.disabled).toBe(true);
    fireEvent.change(input, { target: { value: "  como está a Loja?  " } });
    const reads = view.server.count("GET", /^\/api\/nanty\/conversation$/);
    await act(async () => {
      fireEvent.submit(input.closest("form") as HTMLFormElement);
    });
    await settle();
    expect(posted).toEqual([{ text: "como está a Loja?" }]);
    expect(input.value).toBe("");
    expect(view.server.count("GET", /^\/api\/nanty\/conversation$/)).toBeGreaterThan(reads);

    view.server.reply("GET", /^\/api\/nanty\/conversation$/, () => ({
      status: 200,
      body: { messages: [{ id: "r1", at_ms: AT, from: "nanty", channel: "web", text: "A Loja está parada.", reply_to: "x1" }] },
    }));
    await act(async () => {
      view.stream().emit("nanty_changed", { event_type: "nanty_changed", project_id: null, payload: { part: "conversation" } } as unknown as ApiEvent);
    });
    await settle();
    expect(msg("r1").textContent).toContain("A Loja está parada.");
    expect(msg("r1").querySelector("[data-nanty-origin]")?.textContent).toBe("Nanty, na janela · 14:20");
  });

  it("erro no envio: o texto do código, e o campo guarda a mensagem", async () => {
    const view = await open([]);
    view.server.reply("POST", /^\/api\/nanty\/conversation$/, () => ({ status: 422, body: { error: "x", code: "nanty_message_invalid", params: {} } }));
    const input = within(chat()).getByRole("textbox", { name: "Mensagem para o Nanty" }) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "oi" } });
    await act(async () => {
      fireEvent.submit(input.closest("form") as HTMLFormElement);
    });
    await settle();
    expect(chat().querySelector("[data-nanty-send-error]")?.textContent).toBe("a mensagem ao Nanty precisa ter texto, com até 4096 caracteres");
    expect(input.value).toBe("oi");
  });

  it("o gravar aparece desligado, com a dica de que fica para depois", async () => {
    await open([]);
    const record = within(chat()).getByRole("button", { name: "Gravar áudio" }) as HTMLButtonElement;
    expect(record.disabled).toBe(true);
    const hint = document.getElementById(record.getAttribute("aria-describedby") ?? "");
    expect(hint?.textContent).toBe("Gravar áudio pela janela fica para uma próxima versão. Pelo Telegram, mande uma nota de voz.");
    expect(record.parentElement?.getAttribute("title")).toBe(hint?.textContent);
  });

  it("a mensagem que cita o cartão leva os botões dele; decidir ali mostra o resultado na conversa", async () => {
    const view = await open();
    const posted: unknown[] = [];
    view.server.reply("POST", /^\/api\/nanty\/cards\/aaaaaaaaaaaaaaaa\/decide$/, (body) => {
      posted.push(body);
      return { status: 200, body: { outcome: "declined", code: "nanty.outcome.declined", params: {} } };
    });
    const buttons = within(msg("m5")).getAllByRole("button");
    expect(buttons.map((b) => b.textContent)).toEqual(["Pausar a execução em andamento", "Não", "Lembrar depois"]);
    // Na conversa, nenhuma primária (a da janela fica no feed).
    expect(msg("m5").querySelector('[data-variant="primary"]')).toBeNull();
    await act(async () => {
      fireEvent.click(within(msg("m5")).getByRole("button", { name: "Não" }));
    });
    await settle();
    expect(posted).toEqual([{ token: "tok-p", answer: "no" }]);
    expect(chat().querySelector("[data-nanty-notice]")?.textContent).toBe("Combinado, não faço. Só volto a sugerir se algo mudar.");
  });

  it("em en, sem português", async () => {
    playsOgg(true);
    setLanguage("en", { persist: false });
    await open(MESSAGES.filter((m) => m.from === "nanty" && m.text === undefined));
    expect(chat().querySelector(".nd-nanty-chat-head")?.textContent).toBe("Chatthe same as on Telegram");
    expect(msg("m4").querySelector("[data-nanty-origin]")?.textContent).toBe("Nanty, in the window · 02:20 PM");
    expect(portugueseLeftovers(chat())).toEqual([]);
  });
});
