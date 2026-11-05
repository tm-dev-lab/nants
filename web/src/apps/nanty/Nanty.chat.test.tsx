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
    params: {
      lines: [
        { code: "nanty.chat.status_waiting", params: { count: 1 } },
        { code: "nanty.chat.status_planned", params: { count: 2 } },
        { code: "nanty.chat.status_budget_usd", params: { spent: 0.06, budget: 0.5 } },
      ],
    },
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
      "Estou acompanhando os projetos por aqui.1 aviso espera a sua decisão.Além do resumo da manhã, tenho 2 coisas marcadas para depois.Hoje gastei US$ 0,06 dos US$ 0,50 do dia com o modelo.",
    );
    // A transcrição da nota de voz da pessoa é o texto dela.
    expect(msg("m1").querySelector(".nd-nanty-bubble")?.textContent).toBe("olha a Loja pra mim");
  });

  it("spec 139 T05: o texto fixo sai no idioma gravado na mensagem, não no da interface", async () => {
    playsOgg(true);
    const params = { lines: [{ code: "nanty.chat.status_waiting", params: { count: 1 } }] };
    await open([
      { id: "e1", at_ms: AT, from: "nanty", channel: "web", code: "nanty.chat.status", params, lang: "en" },
      { id: "e2", at_ms: AT + 1000, from: "nanty", channel: "web", code: "nanty.chat.status", params, lang: "pt-BR" },
      { id: "e3", at_ms: AT + 2000, from: "nanty", channel: "web", code: "nanty.chat.status", params },
    ]);
    expect(msg("e1").querySelector(".nd-nanty-bubble")?.textContent).toBe("I am keeping an eye on the projects.1 notice is waiting for your decision.");
    expect(msg("e2").querySelector(".nd-nanty-bubble")?.textContent).toBe("Estou acompanhando os projetos por aqui.1 aviso espera a sua decisão.");
    // Sem idioma (as gravadas antes): o da interface.
    expect(msg("e3").querySelector(".nd-nanty-bubble")?.textContent).toBe("Estou acompanhando os projetos por aqui.1 aviso espera a sua decisão.");
  });

  it("spec 141 T10: com o texto do modelo, o balão é só ele; o título vira selo e os avisos guardados, dados", async () => {
    playsOgg(true);
    const lines = [{ code: "nanty.digest.running", params: { count: 1 } }];
    const held = [
      { project_id: "p2", name: "Mini Street Fighter", nants_fault: true, needs_attention: true, notices: [{ kind: "run_blocked", at: "20:15" }] },
      { project_id: "p1", name: "Loja", notices: [{ kind: "task_delivered", at: "22:01" }, { kind: "task_delivered", at: "22:06" }] },
    ];
    await open([
      { id: "d1", at_ms: AT, from: "nanty", channel: "web", code: "nanty.digest.lead", params: { held }, text: "O Mini Street Fighter parou por um defeito do Nants.", lang: "pt-BR" },
      { id: "d3", at_ms: AT + 2000, from: "nanty", channel: "web", code: "nanty.digest.lead", params: { held }, text: "Mini Street Fighter stopped on a Nants defect.", lang: "en" },
      { id: "d2", at_ms: AT + 1000, from: "nanty", channel: "web", code: "nanty.digest.morning", params: { lines }, lang: "pt-BR" },
      { id: "c1", at_ms: AT + 3000, from: "nanty", channel: "web", code: "nanty.chat.confirm", params: {}, text: "Deixei o cartão pronto.", lang: "pt-BR" },
      { id: "c2", at_ms: AT + 4000, from: "nanty", channel: "web", code: "nanty.chat.confirm", params: {}, lang: "pt-BR" },
      { id: "k1", at_ms: AT + 5000, from: "nanty", channel: "telegram", code: "nanty.chat.open_computer", params: {}, text: "Daqui não dá.", lang: "pt-BR" },
    ]);
    const paragraphs = (id: string) => Array.from(msg(id).querySelectorAll(".nd-nanty-bubble p")).map((p) => p.textContent);
    // Revisão 1 (F-01): sem link, a linha curta do que fazer fica depois do texto do modelo.
    expect(paragraphs("k1")).toEqual([
      "Daqui não dá.",
      "Sem um endereço que o seu aparelho alcance: abra o Nants no computador. Para receber o link aqui, entre uma vez por um aparelho autorizado ou preencha o endereço nos Ajustes do Nanty.",
    ]);
    const seal = (id: string) => msg(id).querySelector("[data-nanty-seal]")?.textContent;
    // O texto do modelo sozinho no balão; o título no selo, no idioma da mensagem.
    expect(paragraphs("d1")).toEqual(["O Mini Street Fighter parou por um defeito do Nants."]);
    expect(seal("d1")).toBe("Resumo da manhã");
    expect(paragraphs("d3")).toEqual(["Mini Street Fighter stopped on a Nants defect."]);
    expect(seal("d3")).toBe("Morning summary");
    // Os dados: a contagem, por projeto, com o defeito do Nants marcado, o tipo e a hora.
    const data = msg("d1").querySelector("[data-nanty-held]");
    expect(data?.querySelector("summary")?.textContent).toBe("3 avisos guardados");
    const groups = Array.from(data?.querySelectorAll("[data-nanty-held-project]") ?? []);
    expect(groups.map((g) => g.querySelector(".nd-nanty-held-project")?.textContent)).toEqual(["Mini Street FighterDefeito do Nants", "Loja"]);
    expect(Array.from(groups[1]?.querySelectorAll("li") ?? []).map((li) => li.textContent)).toEqual(["22:01Tarefa entregue", "22:06Tarefa entregue"]);
    expect(msg("d3").querySelector("[data-nanty-held] summary")?.textContent).toBe("3 saved notices");
    expect(msg("d3").querySelector(".nd-nanty-held-fault")?.textContent).toBe("Nants defect");
    // Sem o texto do modelo, a reserva: uma frase por fato, sem selo nem dados.
    expect(paragraphs("d2")).toEqual(["Bom dia. Aqui vai como estão os projetos.", "1 projeto está rodando."]);
    expect(msg("d2").querySelector("[data-nanty-seal], [data-nanty-held]")).toBeNull();
    // Com o texto do modelo, nenhuma frase fixa fecha; sem ele, o servidor sozinho.
    expect(paragraphs("c1")).toEqual(["Deixei o cartão pronto."]);
    expect(paragraphs("c2")).toEqual(["Confirme no cartão: nada é feito sem o seu toque."]);
  });

  it("spec 141 T10: a reserva do resumo leva o projeto na frente de cada aviso guardado", async () => {
    playsOgg(true);
    const why = [
      { code: "nanty.digest.held", params: { count: 2 } },
      { code: "nanty.why.task_delivered", params: { since: "02:00", project_name: "Loja" } },
      { code: "nanty.why.disk_low", params: { since: "03:00", min_gb: 10 } },
    ];
    await open([{ id: "r1", at_ms: AT, from: "nanty", channel: "web", code: "nanty.digest.morning", params: { why }, lang: "pt-BR" }]);
    const paragraphs = Array.from(msg("r1").querySelectorAll(".nd-nanty-bubble p")).map((p) => p.textContent);
    expect(paragraphs).toEqual([
      "Bom dia. Aqui vai como estão os projetos.",
      "Guardei 2 avisos para este resumo:",
      "Loja: Uma tarefa foi entregue às 02:00.",
      "O espaço livre em disco ficou abaixo de 10 GB às 03:00.",
    ]);
  });

  it("spec 140 T12; spec 141 T10: o cenário abre com o selo curto da análise automática, no idioma dele, e sem botões", async () => {
    playsOgg(true);
    await open([
      { id: "s1", at_ms: AT, from: "nanty", channel: "web", code: "nanty.scenario.lead", params: {}, text: "A tarefa parou nos testes.", lang: "pt-BR", project_ids: ["p1"] },
      { id: "s2", at_ms: AT + 1000, from: "nanty", channel: "web", code: "nanty.scenario.lead", params: {}, text: "The task stopped at the tests.", lang: "en" },
    ]);
    const paragraphs = (id: string) => Array.from(msg(id).querySelectorAll(".nd-nanty-bubble p")).map((p) => p.textContent);
    expect(paragraphs("s1")).toEqual(["A tarefa parou nos testes."]);
    expect(msg("s1").querySelector("[data-nanty-seal]")?.textContent).toBe("Análise automática do Nanty");
    expect(paragraphs("s2")).toEqual(["The task stopped at the tests."]);
    expect(msg("s2").querySelector("[data-nanty-seal]")?.textContent).toBe("Automatic analysis by Nanty");
    expect(msg("s1").querySelectorAll("button")).toHaveLength(0);
  });

  it("spec 140 T12, revisão 1: o cenário recusado pela guarda vira a linha fixa e a frase do servidor", async () => {
    playsOgg(true);
    await open([{ id: "w1", at_ms: AT, from: "nanty", channel: "web", code: "nanty.scenario.lead", params: { lines: [{ code: "nanty.scenario.withheld", params: {} }] }, lang: "pt-BR" }]);
    const paragraphs = Array.from(msg("w1").querySelectorAll(".nd-nanty-bubble p")).map((p) => p.textContent);
    expect(msg("w1").querySelector("[data-nanty-seal]")?.textContent).toBe("Análise automática do Nanty");
    expect(paragraphs).toEqual(["A análise automática deste aviso não pôde ser mostrada aqui. Se quiser, peça na conversa que eu explico o caso."]);
  });

  it("spec 141 T06: a decisão, a resolução e o vencimento de um aviso aparecem como selo, sem balão, nos dois idiomas", async () => {
    playsOgg(true);
    const events = [
      { id: "e1", at_ms: AT, from: "nanty", channel: "telegram", code: "nanty.event.decided.telegram", params: { time: "20:13", answer: "yes" }, project_id: "p1", notices: [{ card_id: "aaaaaaaaaaaaaaaa", project_id: "p1", signal: "plan_waiting", since: "20:11" }] },
      { id: "e2", at_ms: AT + 1000, from: "nanty", channel: "web", code: "nanty.event.resolved", params: { time: "20:30" } },
      { id: "e3", at_ms: AT + 2000, from: "nanty", channel: "web", code: "nanty.event.expired", params: { time: "21:00" } },
    ];
    await open(events);
    const seal = (id: string) => chat().querySelector(`[data-nanty-event][data-nanty-msg="${id}"]`);
    expect(seal("e1")?.textContent).toBe("Você decidiu pelo Telegram, às 20:13: sim.");
    expect(seal("e2")?.textContent).toBe("Aviso resolvido às 20:30: a situação não aparece mais.");
    expect(seal("e3")?.textContent).toBe("Aviso expirado às 21:00.");
    expect(chat().querySelectorAll(".nd-nanty-bubble")).toHaveLength(0);
    cleanup();
    setLanguage("en", { persist: false });
    await open(events);
    expect(seal("e1")?.textContent).toBe("You decided on Telegram, at 08:13 PM: yes.");
    expect(portugueseLeftovers(chat())).toEqual([]);
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

  it("spec 139 T08: a resposta da janela e o aviso têm o tocador junto do texto, sem tocar sozinho", async () => {
    playsOgg(true);
    const play = vi.spyOn(HTMLMediaElement.prototype, "play");
    try {
      await open([
        { id: "w1", at_ms: AT, from: "nanty", channel: "web", text: "A Loja segue bem.", lang: "pt-BR", reply_to: "m0", audio: AUDIO },
        { id: "w2", at_ms: AT + 1000, from: "nanty", channel: "telegram", code: "nanty.why.plan_waiting", params: { since: "10:00" }, lang: "pt-BR", card_ids: ["aaaaaaaaaaaaaaaa"], audio: { ...AUDIO, id: "fedcba9876543210" } },
      ]);
      expect(msg("w1").querySelector("[data-nanty-origin]")?.textContent).toBe("Nanty, na janela · voz e texto · 14:20");
      expect(msg("w1").querySelector(".nd-nanty-bubble")?.textContent).toBe("A Loja segue bem.");
      expect(msg("w2").querySelector(".nd-nanty-bubble")?.textContent).toContain("Um plano espera a sua aprovação");
      for (const id of ["w1", "w2"]) {
        const audio = msg(id).querySelector<HTMLAudioElement>('[data-nanty-voice="player"] audio');
        expect(audio?.getAttribute("preload")).toBe("none");
        expect(audio?.hasAttribute("autoplay")).toBe(false);
      }
      expect(play).not.toHaveBeenCalled();
    } finally {
      play.mockRestore();
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
    expect(chat().querySelector("[data-nanty-chat-empty]")?.textContent).toContain("como você falaria com alguém do time");
    expect(chat().querySelector("[data-nanty-chat-empty]")?.textContent).not.toContain("cite o nome");
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

  it("spec 140 T03: a resposta abre a tela uma vez, quando chega, sem link; o botão volta a ela", async () => {
    const opened = (): boolean => document.querySelector('[data-window="execution"]') !== null;
    const old = { id: "o1", at_ms: AT, from: "nanty", channel: "web", code: "nanty.chat.open_screen", params: { project: "Loja", open_project: "p1" }, lang: "pt-BR" };
    const view = await open([old]);
    expect(opened()).toBe(false);
    expect(msg("o1").textContent).toContain("Abrindo o projeto Loja no Nants.");
    expect(msg("o1").querySelector("a")).toBeNull();

    const reply = { id: "o2", at_ms: AT + 1000, from: "nanty", channel: "web", code: "nanty.chat.open_screen", params: { project: "Loja", open_project: "p1" }, lang: "pt-BR" };
    view.server.reply("GET", /^\/api\/nanty\/conversation$/, () => ({ status: 200, body: { messages: [old, reply] } }));
    await act(async () => {
      view.stream().emit("nanty_changed", { event_type: "nanty_changed", project_id: null, payload: { part: "conversation" } } as unknown as ApiEvent);
    });
    await settle();
    expect(opened()).toBe(true);

    await act(async () => activateApp("Nanty"));
    await settle();
    expect(msg("o2").querySelector("[data-nanty-open]")?.textContent).toBe("Abrir o projeto");
  });

  it("spec 140 T03: reabrir a conversa não reabre a tela; o botão da mensagem abre", async () => {
    await open([{ id: "o1", at_ms: AT, from: "nanty", channel: "web", code: "nanty.chat.open_screen", params: { project: "Loja", open_project: "p1" }, lang: "pt-BR" }]);
    expect(document.querySelector('[data-window="execution"]')).toBeNull();
    await act(async () => {
      fireEvent.click(msg("o1").querySelector("[data-nanty-open]") as HTMLButtonElement);
    });
    await settle();
    expect(document.querySelector('[data-window="execution"]')).not.toBeNull();
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
    expect(chat().querySelector("[data-nanty-notice]")?.textContent).toBe("Combinado, não faço.");
  });

  it("spec 140 T07; spec 141 T10: a resposta que guardou na memória é o texto do modelo, sem o rodapé fixo, e o botão de apagar uma vez só", async () => {
    const forget = {
      id: "ffffffffffffffff",
      project_id: null,
      group: "*:memory_forget:0a1b2c3d4e5f6071",
      code: "nanty.card.memory_forget",
      params: { note: "Disco", entry_id: "0a1b2c3d4e5f6071" },
      action: "memory_forget",
      token: "tok-f",
      created_at_ms: AT,
      expires_at_ms: Date.now() + 3600_000,
    };
    const saved = {
      id: "s1",
      at_ms: AT,
      from: "nanty",
      channel: "web",
      text: "Anotado.",
      code: "nanty.chat.memory_saved",
      params: { title: "Disco" },
      lang: "pt-BR",
      reply_to: "x",
      card_ids: ["ffffffffffffffff"],
    };
    const view = await open([saved], { ...NANTY_EMPTY_FEED, memory: [forget] });
    const footer = "Guardei na memória: Disco. Se preferir que não, toque em apagar.";
    expect(msg("s1").textContent).not.toContain(footer);
    expect(Array.from(msg("s1").querySelectorAll(".nd-nanty-bubble p")).map((p) => p.textContent)).toEqual(["Anotado."]);
    expect(within(msg("s1")).getAllByRole("button").map((b) => b.textContent)).toEqual(["Apagar da memória"]);
    const posted: unknown[] = [];
    view.server.reply("POST", /^\/api\/nanty\/cards\/ffffffffffffffff\/decide$/, (body) => {
      posted.push(body);
      return { status: 200, body: { outcome: "memory_forgotten", code: "nanty.outcome.memory_forgotten", params: {} } };
    });
    await act(async () => {
      fireEvent.click(within(msg("s1")).getByRole("button", { name: "Apagar da memória" }));
    });
    await settle();
    expect(posted).toEqual([{ token: "tok-f", answer: "yes" }]);
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
