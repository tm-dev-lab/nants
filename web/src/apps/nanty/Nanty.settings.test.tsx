/*
 * Regras e Ajustes do Nanty na janela (spec 138 T11): o seletor de três posições e a gravação do
 * nível, a faixa "só você" travada, os ajustes gravados só no que mudou (com o erro traduzido), o
 * orçamento sem preço, a hora do Nants, o "Mudar horário" do feed, o fluxo do Telegram com a API
 * simulada (o token nunca aparece inteiro, o código, os estados, o teste e desparear) e a linha do
 * Nanty em Configurações > Agentes.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { ConnectionView } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { activateApp, NANTY_CONFIG, NANTY_EMPTY_FEED, NANTY_STATUS, NANTY_TELEGRAM, renderShell, settle, type RenderShellOptions } from "../../desktop/shell-test-utils";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { makeProject, makeProjectState } from "../../test-utils";

const P1 = makeProject("p1", "Loja");
const P2 = makeProject("p2", "Blog");
const CONNS: ConnectionView[] = [
  { id: "gem", name: "Gemini pessoal", base_url: "https://generativelanguage.googleapis.com", api_key_masked: "********wxyz", kind: "google_gemini" },
  { id: "oai", name: "OpenAI direto", base_url: "https://api.openai.com/v1", api_key_masked: "********abcd" },
];
const SPACE = { url: "/?space=p1", projects: [P1, P2], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) }, connections: CONNS };
/** Um token com a forma do BotFather, montado na hora (nada com cara de segredo no fonte). */
const TOKEN = `${String(7_700_123_456)}:${"AAH".concat("x9TokenSecretoDoBot", "Q7z")}`;
const MASKED = "********oQ7z";

afterEach(() => {
  cleanup();
  setLanguage("pt-BR", { persist: false });
});

function nantyWindow(): HTMLElement {
  const win = document.querySelector<HTMLElement>('[data-window="nanty"]');
  if (win === null) throw new Error("janela do Nanty ausente");
  return win;
}

type ShellView = Awaited<ReturnType<typeof renderShell>>;

/** A lista de modelos da conexão, para os combobox: uma rota, antes de abrir a janela. */
function modelsReply(view: ShellView): void {
  view.server.reply("GET", /^\/api\/config\/connections\/[^/]+\/models/, () => ({
    status: 200,
    body: { models: [{ id: "gemini-3.8-flash" }, { id: "gemini-3.8-flash-tts" }, { id: "gemini-3.8-flash-lite-tts" }], truncated: false },
  }));
}

async function open(tab: "Regras" | "Ajustes", extra: RenderShellOptions["nanty"] = {}, before?: (view: ShellView) => void, connections: ConnectionView[] = CONNS) {
  const view = await renderShell({ ...SPACE, connections, nanty: { config: NANTY_CONFIG, telegram: NANTY_TELEGRAM, ...extra } });
  before?.(view);
  await act(async () => activateApp("Nanty"));
  await settle();
  const win = nantyWindow();
  await act(async () => {
    fireEvent.click(within(win).getByRole("tab", { name: tab }));
  });
  await settle();
  return { view, win };
}

function catalogWith(id: string, level: string) {
  return NANTY_CONFIG.catalog.map((e) => (e.id === id ? { ...e, level } : e));
}

/** O DOM inteiro serializado (texto, atributos e valores dos campos). */
function serializedDom(): string {
  const values = [...document.querySelectorAll("input")].map((i) => i.value).join("\n");
  return `${new XMLSerializer().serializeToString(document.body)}\n${values}`;
}

describe("Regras", () => {
  it("o seletor de três posições por ação, os grupos, a faixa travada e o texto do topo", async () => {
    const { win } = await open("Regras");
    const rules = win.querySelector<HTMLElement>("[data-nanty-rules]") as HTMLElement;
    const intro = rules.querySelector("[data-nanty-rules-intro]")?.textContent ?? "";
    expect(intro).toContain("sempre volta como cartão para confirmar");
    expect(rules.textContent).not.toMatch(/mais restritiva/i);
    expect([...rules.querySelectorAll("[data-nanty-group]")].map((g) => g.getAttribute("data-nanty-group"))).toEqual(["run", "connections", "machine", "person"]);
    for (const id of ["approve_plan", "resume_run", "retry_provider", "pause_run", "swap_reserve", "clean_orphans"]) {
      const group = within(rules).getByRole("radiogroup", { name: (rules.querySelector(`[data-nanty-rule="${id}"] .nd-nanty-rule-name`)?.textContent ?? "") });
      expect(within(group).getAllByRole("radio").map((r) => r.closest("label")?.textContent)).toEqual(["Desligada", "Pergunta antes", "Automática"]);
    }
    // O nível de cada uma, o do catálogo: a pausa da T08 em "pergunta antes".
    const checked = (id: string) => rules.querySelector(`[data-nanty-levels="${id}"] [data-checked="true"]`)?.getAttribute("data-level");
    expect([checked("approve_plan"), checked("retry_provider"), checked("pause_run")]).toEqual(["ask", "auto", "ask"]);
    expect(rules.querySelector('[data-nanty-rule="retry_provider"]')?.textContent).toContain("Até 2 vezes, com espera entre elas.");
    // Sem reserva escolhida, a troca diz isso.
    expect(rules.querySelector('[data-nanty-rule="swap_reserve"]')?.textContent).toContain("Nenhuma reserva escolhida ainda.");
    // A faixa "só você": travada, com o motivo e sem seletor.
    for (const id of ["allow_command_always", "change_channel"]) {
      const row = rules.querySelector<HTMLElement>(`[data-nanty-rule="${id}"]`) as HTMLElement;
      expect(row.getAttribute("data-locked")).toBe("true");
      expect(row.querySelectorAll("input")).toHaveLength(0);
      expect(row.textContent).toContain("Travada: só você faz isso, na tela");
    }
    expect(rules.querySelector('[data-nanty-rule="change_channel"]')?.textContent).toContain("Só pela aba Ajustes.");
    expect(findForbiddenGlyphs(rules)).toEqual([]);
  });

  it("mudar um nível grava pela rota; o erro sai com o código traduzido e o nível fica", async () => {
    const { view, win } = await open("Regras");
    const sent: unknown[] = [];
    view.server.reply("PUT", /^\/api\/nanty\/actions\/retry_provider$/, (body) => {
      sent.push(body);
      return { status: 200, body: { ...NANTY_CONFIG, catalog: catalogWith("retry_provider", "off") } };
    });
    const row = win.querySelector<HTMLElement>('[data-nanty-levels="retry_provider"]') as HTMLElement;
    await act(async () => {
      fireEvent.click(within(row).getByRole("radio", { name: "Desligada" }));
    });
    await settle();
    expect(sent).toEqual([{ level: "off" }]);
    expect(row.querySelector('[data-checked="true"]')?.getAttribute("data-level")).toBe("off");
    expect(win.querySelector("[data-nanty-notice]")?.textContent).toBe("Nível gravado: Tentar de novo depois de uma falha passageira do provedor, Desligada.");

    view.server.reply("PUT", /^\/api\/nanty\/actions\/approve_plan$/, () => ({ status: 409, body: { error: "x", code: "nanty_action_locked", params: {} } }));
    const approve = win.querySelector<HTMLElement>('[data-nanty-levels="approve_plan"]') as HTMLElement;
    await act(async () => {
      fireEvent.click(within(approve).getByRole("radio", { name: "Automática" }));
    });
    await settle();
    expect(win.querySelector("[data-nanty-notice]")?.textContent).toBe("O nível não foi gravado: esta ação é só sua: o nível dela não muda");
    expect(approve.querySelector('[data-checked="true"]')?.getAttribute("data-level")).toBe("ask");
  });

  it("em inglês, os mesmos textos pelo dicionário", async () => {
    setLanguage("en", { persist: false });
    const { win } = await open("Rules" as "Regras");
    const rules = win.querySelector<HTMLElement>("[data-nanty-rules]") as HTMLElement;
    expect(rules.querySelector("[data-nanty-rules-intro]")?.textContent).toContain("always comes back as a card to confirm");
    expect(rules.textContent).toContain("Up to 2 times, with a wait between them.");
    expect(rules.textContent).toContain("Ask first");
  });
});

describe("Ajustes", () => {
  it("grava só o que mudou e mostra o erro com o código traduzido", async () => {
    const { view, win } = await open("Ajustes");
    const panel = win.querySelector<HTMLElement>("[data-nanty-settings]") as HTMLElement;
    const save = panel.querySelector<HTMLButtonElement>("[data-nanty-save]") as HTMLButtonElement;
    expect(save.disabled).toBe(true);
    const sent: unknown[] = [];
    view.server.reply("PUT", /^\/api\/nanty\/config$/, (body) => {
      sent.push(body);
      return { status: 200, body: { ...NANTY_CONFIG, digest_time: "09:30", daily_budget_usd: 1.25, connection_id: "gem", model: "gemini-flash" } };
    });
    await act(async () => {
      fireEvent.change(within(panel).getByLabelText("Resumo da manhã"), { target: { value: "09:30" } });
      fireEvent.change(within(panel).getByLabelText("Orçamento em US$"), { target: { value: "1.25" } });
      fireEvent.change(within(panel).getByLabelText("Conexão do modelo"), { target: { value: "gem" } });
    });
    await act(async () => {
      fireEvent.change(panel.querySelector('[data-nanty-pref="model"] input[type="text"]') as HTMLInputElement, { target: { value: " gemini-flash " } });
    });
    expect(save.disabled).toBe(false);
    await act(async () => {
      fireEvent.click(save);
    });
    await settle();
    expect(sent).toEqual([{ connection_id: "gem", model: "gemini-flash", daily_budget_usd: 1.25, digest_time: "09:30" }]);
    expect(panel.querySelector("[data-nanty-save-notice]")?.textContent).toBe("Ajustes salvos.");
    expect(save.disabled).toBe(true);

    // A reserva, a voz, os projetos e a espera do Telegram, também só no que mudou.
    sent.length = 0;
    view.server.reply("PUT", /^\/api\/nanty\/config$/, (body) => {
      sent.push(body);
      return { status: 422, body: { error: "x", code: "nanty_config_invalid", params: { field: "reserve.developer" } } };
    });
    const dev = panel.querySelector<HTMLElement>('[data-nanty-reserve="developer"]') as HTMLElement;
    expect(within(dev).getByRole("combobox", { name: "Modelo de reserva do papel Desenvolvedor" }).getAttribute("placeholder")).toBe("Modelo");
    await act(async () => {
      fireEvent.change(within(dev).getByRole("combobox", { name: "Conexão de reserva do papel Desenvolvedor" }), { target: { value: "oai" } });
    });
    await act(async () => {
      fireEvent.change(within(dev).getByRole("combobox", { name: "Modelo de reserva do papel Desenvolvedor" }), { target: { value: "gpt-x" } });
      fireEvent.click(within(panel).getByRole("switch", { name: "Responder em voz" }));
      fireEvent.click(within(panel).getByRole("switch", { name: "Esperar com uma aba do Nants aberta" }));
      fireEvent.click(within(panel).getByRole("checkbox", { name: "Blog" }));
    });
    await act(async () => {
      fireEvent.click(save);
    });
    await settle();
    expect(sent).toEqual([
      {
        excluded_projects: ["p2"],
        reserve: { architect: { connection_id: "", model: "" }, developer: { connection_id: "oai", model: "gpt-x" }, reviewer: { connection_id: "", model: "" } },
        voice: { enabled: true },
        telegram: { hold_while_visible: false },
      },
    ]);
    expect(panel.querySelector("[data-nanty-save-notice]")?.textContent).toBe("Os ajustes não foram salvos: o ajuste reserve.developer do Nanty tem um valor fora do aceito");
  });

  it("spec 139 T05: o seletor do idioma do Nanty substitui o atalho da Aparência e grava só o idioma", async () => {
    const { view, win } = await open("Ajustes");
    const panel = win.querySelector<HTMLElement>("[data-nanty-settings]") as HTMLElement;
    const card = panel.querySelector<HTMLElement>('[data-nanty-pref="lang"]') as HTMLElement;
    expect(within(card).queryByRole("button")).toBeNull();
    const select = within(card).getByLabelText("Avisos e resumo da manhã") as HTMLSelectElement;
    expect(select.value).toBe("follow");
    expect([...select.options].map((o) => o.textContent)).toEqual(["Seguir a conversa", "Português", "Inglês"]);
    expect(card.querySelector("[data-nanty-lang]")?.textContent).toContain("(português)");
    const sent: unknown[] = [];
    view.server.reply("PUT", /^\/api\/nanty\/config$/, (body) => {
      sent.push(body);
      return { status: 200, body: { ...NANTY_CONFIG, language: "en" } };
    });
    await act(async () => {
      fireEvent.change(select, { target: { value: "en" } });
    });
    expect(card.querySelector("[data-nanty-lang]")).toBeNull();
    await act(async () => {
      fireEvent.click(panel.querySelector("[data-nanty-save]") as HTMLButtonElement);
    });
    await settle();
    expect(sent).toEqual([{ language: "en" }]);
    expect((within(card).getByLabelText("Avisos e resumo da manhã") as HTMLSelectElement).value).toBe("en");
  });

  it("número fora da faixa trava o salvar com a dica", async () => {
    const { win } = await open("Ajustes");
    const panel = win.querySelector<HTMLElement>("[data-nanty-settings]") as HTMLElement;
    await act(async () => {
      fireEvent.change(within(panel).getByLabelText("Orçamento em US$"), { target: { value: "5000" } });
    });
    expect(panel.textContent).toContain("Use um número de 0 a 1.000.");
    expect((panel.querySelector("[data-nanty-save]") as HTMLButtonElement).disabled).toBe(true);
  });

  it("orçamento sem preço conta tokens; a barra do gasto; a hora do Nants no formato da tela", async () => {
    const budget = { mode: "tokens", spent_usd: 0, budget_usd: 0.5, tokens: 50_000, budget_tokens: 200_000, unpriced_turns: 3, unpriced: true, exhausted: false };
    const { win } = await open("Ajustes", { status: { ...NANTY_STATUS, model_configured: true, budget } });
    const card = win.querySelector<HTMLElement>('[data-nanty-pref="budget"]') as HTMLElement;
    expect(card.querySelector("[data-nanty-unpriced]")?.textContent).toBe("Sem preço, o orçamento conta tokens.");
    expect(card.querySelector("[data-nanty-spent]")?.textContent).toBe("50.000 de 200.000, em tokens");
    expect(within(card).getByRole("progressbar").getAttribute("aria-valuenow")).toBe("25");
    const hours = win.querySelector<HTMLElement>('[data-nanty-pref="hours"]') as HTMLElement;
    expect(hours.querySelector("[data-nanty-now]")?.textContent).toBe("Hora do Nants agora: 09:41");
    expect(hours.querySelector("[data-nanty-hours-summary]")?.textContent).toBe("Silêncio das 22:00 às 07:00. Resumo às 08:00.");
    // O idioma do Nanty (spec 139 T05): seguir a conversa, com o da interface antes da primeira.
    expect(win.querySelector('[data-nanty-pref="lang"] [data-nanty-lang]')?.textContent).toContain("vale o da interface (português)");
    cleanup();
    setLanguage("en", { persist: false });
    const en = await open("Settings" as "Ajustes");
    const hoursEn = en.win.querySelector<HTMLElement>('[data-nanty-pref="hours"]') as HTMLElement;
    expect(hoursEn.querySelector("[data-nanty-now]")?.textContent).toBe("Nants time now: 09:41 AM");
    expect(hoursEn.querySelector("[data-nanty-hours-summary]")?.textContent).toBe("Quiet from 10:00 PM to 07:00 AM. Summary at 08:00 AM.");
  });

  it('o "Mudar horário" do Previsto abre os Ajustes com o campo do resumo em foco; a hora sai no formato da tela', async () => {
    setLanguage("en", { persist: false });
    const view = await renderShell({ ...SPACE, nanty: { config: { ...NANTY_CONFIG, digest_time: "08:30" }, telegram: NANTY_TELEGRAM, feed: NANTY_EMPTY_FEED } });
    await act(async () => activateApp("Nanty"));
    await settle();
    const win = nantyWindow();
    const planned = win.querySelector<HTMLElement>('[data-nanty-planned="digest"]') as HTMLElement;
    expect(planned.textContent).toContain("08:30 AM");
    await act(async () => {
      fireEvent.click(within(planned).getByRole("button", { name: "Change time" }));
    });
    await settle();
    expect(within(win).getByRole("tab", { name: "Settings" }).getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement?.hasAttribute("data-nanty-digest-time")).toBe(true);
    expect(view.server.count("GET", /^\/api\/nanty\/config$/)).toBeGreaterThan(0);
  });
});

describe("cartão Voz", () => {
  const PLAN: ConnectionView = { id: "plan", name: "Plano ChatGPT", base_url: "https://chatgpt.com/backend-api/codex", api_key_masked: "", kind: "chatgpt_plan" };
  const voiceCard = (win: HTMLElement) => win.querySelector<HTMLElement>('[data-nanty-pref="voice"]') as HTMLElement;
  const state = (card: HTMLElement) => card.querySelector("[data-nanty-voice-state]");
  const withVoice = (voice: object) => ({ config: { ...NANTY_CONFIG, voice: { ...NANTY_CONFIG.voice, ...voice } } });
  const voicesFail = (status: number, code: string, params: object = {}) => (view: ShellView) => view.server.reply("GET", /^\/api\/nanty\/voices/, () => ({ status, body: { error: "x", code, params } }));

  it("um seletor só, com as conexões do Gemini pelo nome e o tipo; modelos e voz à vista; sem a ordem", async () => {
    const { view, win } = await open("Ajustes", {}, modelsReply, [...CONNS, PLAN]);
    const card = voiceCard(win);
    const select = within(card).getByLabelText("Conexão do Gemini para a voz") as HTMLSelectElement;
    expect([...select.options].map((o) => o.textContent)).toEqual(["Automática: a primeira com chave", "Gemini pessoal (Google Gemini)"]);
    expect(card.querySelectorAll("select")).toHaveLength(1);
    expect(card.textContent).not.toMatch(/ordem|tentativa|transcreve os seus/i);
    expect((within(card).getByLabelText("Modelo de fala") as HTMLInputElement).value).toBe("gemini-3.8-flash-tts");
    expect((within(card).getByLabelText("Voz") as HTMLInputElement).value).toBe("Kore");
    expect((within(card).getByLabelText("Modelo de transcrição") as HTMLInputElement).value).toBe("gemini-3.8-flash");
    expect(card.textContent).toContain("cadastradas em Configurações > Conexões");
    // O texto diz o que acontece de fato (T08): o áudio do aviso nasce no Telegram.
    expect(card.textContent).toContain("Na janela, as respostas têm áudio; os avisos saem em áudio no Telegram.");
    // A escolha grava só a conexão, com o campo novo.
    const sent: unknown[] = [];
    view.server.reply("PUT", /^\/api\/nanty\/config$/, (body) => {
      sent.push(body);
      return { status: 200, body: { ...NANTY_CONFIG, voice: { ...NANTY_CONFIG.voice, connection_id: "gem" } } };
    });
    await act(async () => {
      fireEvent.change(select, { target: { value: "gem" } });
    });
    await settle();
    await act(async () => {
      fireEvent.click(win.querySelector("[data-nanty-save]") as HTMLButtonElement);
    });
    await settle();
    expect(sent).toEqual([{ voice: { connection_id: "gem" } }]);
    expect(view.server.count("GET", /^\/api\/nanty\/voices\?connection_id=gem$/)).toBeGreaterThan(0);
  });

  it("a conexão gravada que não serve fica à vista, com o aviso, até a troca", async () => {
    const { win } = await open("Ajustes", withVoice({ connection_id: "oai" }), modelsReply);
    const card = voiceCard(win);
    const select = within(card).getByLabelText("Conexão do Gemini para a voz") as HTMLSelectElement;
    expect([...select.options].map((o) => o.value)).toEqual(["", "gem", "oai"]);
    expect(card.querySelector("[data-nanty-voice-warn]")?.textContent).toBe("A conexão OpenAI direto não é do Google Gemini. Escolha outra na lista.");
    expect(state(card)?.getAttribute("data-nanty-voice-state")).toBe("no_connection");
    await act(async () => {
      fireEvent.change(select, { target: { value: "gem" } });
    });
    await settle();
    expect(card.querySelector("[data-nanty-voice-warn]")).toBeNull();
    expect([...select.options].map((o) => o.value)).toEqual(["", "gem"]);
    expect(state(card)?.getAttribute("data-nanty-voice-state")).toBe("ready");
  });

  it("os estados do servidor: pronta (com a conexão resolvida), sem chave e sem conexão", async () => {
    const ready = await open("Ajustes", {}, modelsReply);
    expect(state(voiceCard(ready.win))?.textContent).toBe("Pronta, pela conexão Gemini pessoal");
    cleanup();
    const noKey = await open("Ajustes", {}, voicesFail(409, "nanty_voice_no_key"));
    expect(state(voiceCard(noKey.win))?.textContent).toBe("Sem chave");
    expect(voiceCard(noKey.win).querySelector('[data-nanty-voice-why="no_key"]')?.textContent).toContain("Coloque a chave em Configurações > Conexões");
    expect(within(voiceCard(noKey.win)).getByLabelText("Conexão do Gemini para a voz")).toBeTruthy();
    cleanup();
    const gone = await open("Ajustes", withVoice({ connection_id: "sumiu" }));
    expect(state(voiceCard(gone.win))?.textContent).toBe("Sem conexão");
    expect(voiceCard(gone.win).querySelector("[data-nanty-voice-warn]")?.textContent).toBe("A conexão sumiu não existe mais. Escolha outra.");
  });

  it("sem conexão do Gemini: explica e o atalho abre Configurações > Conexões", async () => {
    const { view, win } = await open("Ajustes", {}, (v) => {
      v.server.reply("GET", /^\/api\/system\/prereqs$/, () => ({ status: 200, body: [] }));
    }, [CONNS[1] as ConnectionView]);
    const card = voiceCard(win);
    expect(state(card)?.textContent).toBe("Sem conexão");
    expect(card.querySelector('[data-nanty-voice-why="no_connection"]')?.textContent).toContain("você ainda não tem nenhuma");
    expect(within(card).queryByLabelText("Conexão do Gemini para a voz")).toBeNull();
    const shortcut = within(card).getByRole("button", { name: "Criar uma conexão do Gemini" });
    expect(shortcut.getAttribute("data-variant")).toBe("primary");
    await act(async () => {
      fireEvent.click(shortcut);
    });
    await settle();
    const settings = screen.getByRole("region", { name: "Configurações" });
    expect(within(settings).getByRole("tab", { name: /^Conexões/ }).getAttribute("aria-selected")).toBe("true");
    expect(view.server.count("GET", /^\/api\/nanty\/voices/)).toBeGreaterThan(0);
  });

  it("com conexão, o atalho é um botão secundário com ícone e leva ao mesmo lugar", async () => {
    const { win } = await open("Ajustes", {}, (v) => {
      modelsReply(v);
      v.server.reply("GET", /^\/api\/system\/prereqs$/, () => ({ status: 200, body: [] }));
    });
    const shortcut = within(voiceCard(win)).getByRole("button", { name: "Abrir Conexões" });
    // Sem variante é o secundário do `.nd-btn`, igual a "Salvar o token".
    expect(shortcut.classList.contains("nd-btn")).toBe(true);
    expect(shortcut.hasAttribute("data-variant")).toBe(false);
    expect(shortcut.querySelector("svg")).not.toBeNull();
    await act(async () => {
      fireEvent.click(shortcut);
    });
    await settle();
    expect(within(screen.getByRole("region", { name: "Configurações" })).getByRole("tab", { name: /^Conexões/ }).getAttribute("aria-selected")).toBe("true");
  });

  it("a rota de vozes falha: a tela segue usável, com o erro traduzido, o texto livre e os modelos", async () => {
    const { view, win } = await open("Ajustes", {}, (v) => {
      modelsReply(v);
      voicesFail(502, "nanty_voice_list_failed", { reason: "provider_error", connection_id: "gem" })(v);
    });
    const card = voiceCard(win);
    expect(state(card)?.textContent).toBe("Pronta, pela conexão Gemini pessoal");
    expect(card.querySelector("[data-nanty-voices-failed]")?.textContent).toBe("o Google Gemini não devolveu a lista de vozes; tente de novo. Você pode digitar o nome da voz.");
    const voice = within(card).getByLabelText("Voz") as HTMLInputElement;
    await act(async () => {
      fireEvent.change(voice, { target: { value: "Puck" } });
    });
    expect(voice.value).toBe("Puck");
    // A lista de modelos não depende da de vozes.
    await act(async () => {
      fireEvent.click(within(card).getByLabelText("Modelo de fala"));
    });
    expect(within(card).getAllByRole("option").map((o) => o.textContent)).toContain("gemini-3.8-flash-lite-tts");
    // "Atualizar lista" tenta de novo.
    view.server.reply("GET", /^\/api\/nanty\/voices/, () => ({ status: 200, body: { connection_id: "gem", voices: [{ id: "Kore", display_name: "Kore" }] } }));
    const row = within(card).getByLabelText("Voz").closest(".nd-field") as HTMLElement;
    await act(async () => {
      fireEvent.click(within(row).getByRole("button", { name: "Atualizar lista" }));
    });
    await settle();
    expect(card.querySelector("[data-nanty-voices-failed]")).toBeNull();
  });

  it("a chave recusada: não diz pronta, explica onde trocar, e os modelos e o texto livre seguem", async () => {
    const { win } = await open("Ajustes", {}, (v) => {
      modelsReply(v);
      voicesFail(502, "nanty_voice_list_failed", { reason: "auth", connection_id: "gem" })(v);
    });
    const card = voiceCard(win);
    expect(state(card)?.getAttribute("data-nanty-voice-state")).toBe("key_refused");
    expect(state(card)?.textContent).toBe("Chave recusada");
    expect(state(card)?.querySelector(".nd-dot")?.getAttribute("data-tone")).toBe("amber");
    expect(card.querySelector('[data-nanty-voice-why="key_refused"]')?.textContent).toContain("Troque a chave em Configurações > Conexões");
    expect(card.textContent).not.toContain("tente de novo");
    expect(within(card).getByRole("button", { name: "Abrir Conexões" })).toBeTruthy();
    const voice = within(card).getByLabelText("Voz") as HTMLInputElement;
    await act(async () => {
      fireEvent.change(voice, { target: { value: "Puck" } });
    });
    expect(voice.value).toBe("Puck");
    await act(async () => {
      fireEvent.click(within(card).getByLabelText("Modelo de fala"));
    });
    expect(within(card).getAllByRole("option").map((o) => o.textContent)).toContain("gemini-3.8-flash-lite-tts");
  });

  it("em inglês, a chave recusada também vem do dicionário", async () => {
    setLanguage("en", { persist: false });
    const { win } = await open("Settings" as "Ajustes", {}, (v) => {
      modelsReply(v);
      voicesFail(502, "nanty_voice_list_failed", { reason: "auth", connection_id: "gem" })(v);
    });
    const card = voiceCard(win);
    expect(state(card)?.textContent).toBe("Key refused");
    expect(card.querySelector('[data-nanty-voice-why="key_refused"]')?.textContent).toContain("Replace the key in Settings > Connections");
  });

  it("a gravada sumiu: escolher a Automática mostra o que ela resolve, antes e depois de salvar", async () => {
    const { view, win } = await open("Ajustes", withVoice({ connection_id: "sumiu" }), modelsReply);
    const card = voiceCard(win);
    expect(state(card)?.textContent).toBe("Sem conexão");
    expect(view.server.count("GET", /^\/api\/nanty\/voices\?connection_id=sumiu$/)).toBeGreaterThan(0);
    await act(async () => {
      fireEvent.change(within(card).getByLabelText("Conexão do Gemini para a voz"), { target: { value: "" } });
    });
    await settle();
    expect(view.server.count("GET", /^\/api\/nanty\/voices\?connection_id=$/)).toBeGreaterThan(0);
    expect(state(card)?.textContent).toBe("Pronta, pela conexão Gemini pessoal");
    expect(card.querySelector("[data-nanty-voice-warn]")).toBeNull();
    // As vozes e os modelos são os da conexão resolvida.
    await act(async () => {
      fireEvent.click(within(card).getByLabelText("Voz"));
    });
    expect(within(card).getAllByRole("option").map((o) => o.textContent)).toContain("Kore (pt-BR, Firm)");
    expect(view.server.count("GET", /^\/api\/config\/connections\/gem\/models/)).toBeGreaterThan(0);
    await act(async () => {
      fireEvent.blur(within(card).getByLabelText("Voz"));
    });
    const sent: unknown[] = [];
    view.server.reply("PUT", /^\/api\/nanty\/config$/, (body) => {
      sent.push(body);
      return { status: 200, body: NANTY_CONFIG };
    });
    await act(async () => {
      fireEvent.click(win.querySelector("[data-nanty-save]") as HTMLButtonElement);
    });
    await settle();
    expect(sent).toEqual([{ voice: { connection_id: "" } }]);
    expect(state(card)?.textContent).toBe("Pronta, pela conexão Gemini pessoal");
    expect((within(card).getByLabelText("Conexão do Gemini para a voz") as HTMLSelectElement).value).toBe("");
  });

  it("sem resposta do servidor: nenhum estado inventado, e o erro traduzido", async () => {
    const { win } = await open("Ajustes", {}, voicesFail(500, "nanty_config_invalid", { field: "voice" }));
    const card = voiceCard(win);
    expect(state(card)).toBeNull();
    expect(card.querySelector("[data-nanty-voices-failed]")?.textContent).toContain("Você pode digitar o nome da voz.");
  });
});

describe("combobox de modelos nos Ajustes", () => {
  const options = (root: HTMLElement) => within(within(root).getByRole("listbox")).getAllByRole("option").map((o) => o.textContent);
  const choose = async (input: HTMLElement, name: string, root: HTMLElement) => {
    await act(async () => {
      fireEvent.click(input);
    });
    await act(async () => {
      fireEvent.click(within(root).getByRole("option", { name }));
    });
  };

  it("Modelo, Reserva, fala e transcrição: a lista completa, sem filtro por nome, e o texto livre", async () => {
    const config = { ...NANTY_CONFIG, connection_id: "gem", model: "gemini-3.8-flash", reserve: { developer: { connection_id: "gem", model: "gemini-3.8-flash" } } };
    const { view, win } = await open("Ajustes", { config }, modelsReply);
    const panel = win.querySelector<HTMLElement>("[data-nanty-settings]") as HTMLElement;
    const all = ["gemini-3.8-flash", "gemini-3.8-flash-tts", "gemini-3.8-flash-lite-tts"];
    const model = panel.querySelector<HTMLElement>('[data-nanty-pref="model"]') as HTMLElement;
    const reserve = panel.querySelector<HTMLElement>('[data-nanty-reserve="developer"]') as HTMLElement;
    const voice = panel.querySelector<HTMLElement>('[data-nanty-pref="voice"]') as HTMLElement;
    // A fala tem a lista própria, da rota de vozes: a da conexão é só de texto.
    const speech = ["gemini-3.8-flash-lite-tts", "gemini-3.8-flash-tts"];
    for (const [root, input, expected] of [
      [model, within(model).getByRole("combobox", { name: "Modelo" }), all],
      [reserve, within(reserve).getByRole("combobox", { name: "Modelo de reserva do papel Desenvolvedor" }), all],
      [voice, within(voice).getByRole("combobox", { name: "Modelo de fala" }), speech],
      [voice, within(voice).getByRole("combobox", { name: "Modelo de transcrição" }), all],
    ] as const) {
      await act(async () => {
        fireEvent.click(input);
      });
      expect(options(root)).toEqual(expected);
      await act(async () => {
        fireEvent.blur(input);
      });
    }
    // A mesma conexão: uma carga só, compartilhada entre os quatro campos.
    expect(view.server.count("GET", /^\/api\/config\/connections\/gem\/models/)).toBe(1);
    // A lista de vozes vem da rota, com o rótulo; o valor gravado é o id.
    const voiceInput = within(voice).getByLabelText("Voz") as HTMLInputElement;
    await act(async () => {
      fireEvent.change(voiceInput, { target: { value: "" } });
    });
    await choose(voiceInput, "Kore (pt-BR, Firm)", voice);
    expect(voiceInput.value).toBe("Kore");
    await choose(within(voice).getByLabelText("Modelo de fala"), "gemini-3.8-flash-lite-tts", voice);
    const sent: unknown[] = [];
    view.server.reply("PUT", /^\/api\/nanty\/config$/, (body) => {
      sent.push(body);
      return { status: 200, body: config };
    });
    await act(async () => {
      fireEvent.change(within(model).getByRole("combobox", { name: "Modelo" }), { target: { value: "modelo-que-nao-esta-na-lista" } });
    });
    await act(async () => {
      fireEvent.click(panel.querySelector("[data-nanty-save]") as HTMLButtonElement);
    });
    await settle();
    expect(sent).toEqual([{ model: "modelo-que-nao-esta-na-lista", voice: { speech_model: "gemini-3.8-flash-lite-tts" } }]);
  });

  it("a lista falha: o campo segue aceitando texto, com o erro; sem conexão, fica travado", async () => {
    const { view, win } = await open("Ajustes", {}, (v) => v.server.reply("GET", /^\/api\/config\/connections\/[^/]+\/models/, () => ({ status: 200, body: { models: [], error: "http_error", status: 401 } })));
    const card = win.querySelector<HTMLElement>('[data-nanty-pref="model"]') as HTMLElement;
    const model = within(card).getByRole("combobox", { name: "Modelo" }) as HTMLInputElement;
    expect(model.disabled).toBe(true);
    await act(async () => {
      fireEvent.change(within(card).getByLabelText("Conexão do modelo"), { target: { value: "oai" } });
    });
    await settle();
    expect(model.disabled).toBe(false);
    expect(card.querySelector(".nd-field-warn")).not.toBeNull();
    await act(async () => {
      fireEvent.change(model, { target: { value: "gpt-x" } });
    });
    expect(model.value).toBe("gpt-x");
    expect(view.server.count("GET", /^\/api\/config\/connections\/oai\/models/)).toBeGreaterThan(0);
  });
});

describe("Telegram nos Ajustes", () => {
  it("token mascarado, código e link, teste e desparear; o token nunca fica no DOM", async () => {
    const { view, win } = await open("Ajustes");
    const card = win.querySelector<HTMLElement>('[data-nanty-pref="telegram"]') as HTMLElement;
    expect(card.querySelector("[data-nanty-tg-state]")?.getAttribute("data-nanty-tg-state")).toBe("not_configured");
    expect(card.querySelector("[data-nanty-steps]")?.textContent).toContain("/newbot");
    expect(card.querySelector("[data-nanty-privacy]")?.textContent).toBe("As mensagens passam pelos servidores do Telegram. Por isso vão só resumos, nunca código, logs ou segredos.");
    // T12: o Nanty dorme com o Nants fechado, e o clique antigo nunca decide.
    expect(card.querySelector("[data-nanty-sleep]")?.textContent).toBe(
      "O Nanty só trabalha com o Nants aberto. Com o Nants fechado, ele dorme: o que você manda espera no Telegram e volta como cartão antigo, e um botão tocado nesse meio tempo nunca decide nada.",
    );
    expect(card.querySelector("[data-nanty-pair]")).toBeNull();

    let tg: Record<string, unknown> = { health: "connected", bot_username: "NantsNantyBot", bot_token_masked: MASKED, configured: true, paired: false, pairing: { state: "closed" } };
    const posted: { url: string; body: unknown }[] = [];
    view.server.reply("POST", /^\/api\/nanty\/telegram\/token$/, (body) => {
      posted.push({ url: "token", body });
      return { status: 200, body: tg };
    });
    view.server.reply("GET", /^\/api\/nanty\/telegram$/, () => ({ status: 200, body: tg }));
    const input = card.querySelector<HTMLInputElement>("[data-nanty-token]") as HTMLInputElement;
    expect(input.type).toBe("password");
    await act(async () => {
      fireEvent.change(input, { target: { value: ` ${TOKEN} ` } });
    });
    await act(async () => {
      fireEvent.click(within(card).getByRole("button", { name: "Salvar o token" }));
    });
    await settle();
    expect(posted).toEqual([{ url: "token", body: { token: TOKEN } }]);
    expect(input.value).toBe("");
    expect(input.placeholder).toBe(`Salvo: ${MASKED}. Cole outro para trocar.`);
    expect(card.querySelector("[data-nanty-tg-state]")?.getAttribute("data-nanty-tg-state")).toBe("unpaired");
    expect(card.querySelector("[data-nanty-tg-detail]")?.textContent).toBe("Bot @NantsNantyBot conectado. Falta parear com o seu chat.");
    expect(serializedDom()).not.toContain(TOKEN);
    expect(serializedDom()).not.toContain("x9TokenSecretoDoBot");

    // O código e o link de pareamento, com a validade e um código novo.
    const code = "AbCdEfGh12345_-z";
    view.server.reply("POST", /^\/api\/nanty\/telegram\/pair$/, () => {
      tg = { ...tg, pairing: { state: "open", code, link: `https://t.me/NantsNantyBot?start=${code}`, seconds_to_next: 120, expires_in: 600 } };
      return { status: 200, body: tg };
    });
    await act(async () => {
      fireEvent.click(within(card).getByRole("button", { name: "Gerar o código de pareamento" }));
    });
    await settle();
    expect(card.querySelector("[data-nanty-code]")?.textContent).toBe(code);
    expect(card.querySelector("[data-nanty-link]")?.getAttribute("href")).toBe(`https://t.me/NantsNantyBot?start=${code}`);
    expect(card.querySelector("[data-nanty-validity]")?.textContent).toBe("A janela fecha em 10:00. O código troca em 2:00.");
    expect(within(card).getByRole("button", { name: "Gerar um código novo" })).toBeTruthy();
    // Um link fora da forma não aparece.
    tg = { ...tg, pairing: { state: "open", code, link: "https://evil.example/x", seconds_to_next: 100, expires_in: 500 } };
    view.server.reply("POST", /^\/api\/nanty\/telegram\/pair$/, () => ({ status: 200, body: tg }));
    await act(async () => {
      fireEvent.click(within(card).getByRole("button", { name: "Gerar um código novo" }));
    });
    await settle();
    expect(card.querySelector("[data-nanty-link]")).toBeNull();

    // Pareado: teste e desparear.
    tg = { ...tg, paired: true, pairing: { state: "closed" } };
    view.server.reply("POST", /^\/api\/nanty\/telegram\/test$/, () => ({ status: 200, body: tg }));
    await act(async () => {
      view.stream().emit("nanty_changed", { event_type: "nanty_changed", project_id: null, payload: { part: "status" } } as never);
    });
    view.server.reply("GET", /^\/api\/nanty\/status$/, () => ({ status: 200, body: { ...NANTY_STATUS, telegram: "connected", telegram_configured: true, telegram_paired: true } }));
    await act(async () => {
      view.stream().emit("nanty_changed", { event_type: "nanty_changed", project_id: null, payload: { part: "status" } } as never);
    });
    await settle();
    expect(card.querySelector("[data-nanty-tg-state]")?.getAttribute("data-nanty-tg-state")).toBe("connected");
    expect(card.querySelector("[data-nanty-tg-detail]")?.textContent).toBe("Bot @NantsNantyBot, pareado só com o seu chat.");
    expect(card.querySelector("[data-nanty-code]")).toBeNull();
    await act(async () => {
      fireEvent.click(within(card).getByRole("button", { name: "Enviar mensagem de teste" }));
    });
    await settle();
    expect(card.querySelector("[data-nanty-tg-notice]")?.textContent).toBe("Mensagem de teste enviada.");
    view.server.reply("POST", /^\/api\/nanty\/telegram\/unpair$/, () => {
      tg = { ...tg, paired: false };
      return { status: 200, body: tg };
    });
    await act(async () => {
      fireEvent.click(within(card).getByRole("button", { name: "Desparear" }));
    });
    await settle();
    expect(card.querySelector("[data-nanty-tg-notice]")?.textContent).toBe("Despareado. Gere um código novo para parear de novo.");
    expect(card.querySelector("[data-nanty-tg-state]")?.getAttribute("data-nanty-tg-state")).toBe("unpaired");
    expect(view.server.count("POST", /^\/api\/nanty\/telegram\/(test|unpair)$/)).toBe(2);
    expect(serializedDom()).not.toContain("x9TokenSecretoDoBot");
  });

  it("os estados: token inválido, conflito (um bot por instalação), rede e limite; o erro do envio traduzido", async () => {
    const base = { bot_username: "NantsNantyBot", bot_token_masked: MASKED, configured: true, paired: false, pairing: { state: "closed" } };
    const cases: [string, string][] = [
      ["invalid_token", "O Telegram recusou o token."],
      ["conflict", "um bot por instalação"],
      ["network_error", "Sem conexão com o Telegram agora."],
      ["rate_limited", "O Telegram pediu para esperar um pouco."],
    ];
    for (const [health, text] of cases) {
      const { win } = await open("Ajustes", { telegram: { ...base, health } });
      const card = win.querySelector<HTMLElement>('[data-nanty-pref="telegram"]') as HTMLElement;
      expect(card.querySelector("[data-nanty-tg-state]")?.getAttribute("data-nanty-tg-state")).toBe(health);
      expect(card.querySelector("[data-nanty-tg-detail]")?.textContent).toContain(text);
      cleanup();
    }
    const { view, win } = await open("Ajustes", { telegram: { ...base, health: "connected", paired: true } });
    view.server.reply("POST", /^\/api\/nanty\/telegram\/test$/, () => ({ status: 502, body: { error: "x", code: "nanty_telegram_send_failed", params: {} } }));
    const card = win.querySelector<HTMLElement>('[data-nanty-pref="telegram"]') as HTMLElement;
    await act(async () => {
      fireEvent.click(within(card).getByRole("button", { name: "Enviar mensagem de teste" }));
    });
    await settle();
    expect(card.querySelector("[data-nanty-tg-notice]")?.textContent).toBe("Não deu certo: o Telegram não recebeu a mensagem de teste; confira a conexão");
  });
});

describe("em inglês", () => {
  it("Regras e Ajustes sem português na tela", async () => {
    setLanguage("en", { persist: false });
    const telegram = { health: "connected", bot_username: "NantsNantyBot", bot_token_masked: MASKED, configured: true, paired: false, pairing: { state: "open", code: "AbCdEfGh12345_-z", link: "https://t.me/NantsNantyBot?start=AbCdEfGh12345_-z", seconds_to_next: 60, expires_in: 300 } };
    const budget = { mode: "tokens", spent_usd: 0, budget_usd: 0.5, tokens: 10, budget_tokens: 200_000, unpriced_turns: 1, unpriced: true, exhausted: false };
    const config = { ...NANTY_CONFIG, lang: "en", voice: { ...NANTY_CONFIG.voice, connection_id: "gem" } };
    const { win } = await open("Settings" as "Ajustes", { config, telegram, status: { ...NANTY_STATUS, budget } }, modelsReply);
    const leftovers = (root: HTMLElement) => portugueseLeftovers(root).filter((text) => !/^(Loja|Blog|Gemini pessoal|Gemini pessoal \(Google Gemini\)|OpenAI direto)$/.test(text));
    expect(leftovers(win)).toEqual([]);
    expect(findForbiddenGlyphs(win)).toEqual([]);
    await act(async () => {
      fireEvent.click(within(win).getByRole("tab", { name: "Rules" }));
    });
    await settle();
    expect(leftovers(win)).toEqual([]);
  });
});

describe("rolagem entre as abas", () => {
  it("trocar de aba volta ao topo; o Mudar horário leva ao campo do resumo", async () => {
    const { win } = await open("Regras");
    const panel = within(win).getByRole("tabpanel");
    panel.scrollTop = 900;
    expect(panel.scrollTop).toBe(900);
    await act(async () => {
      fireEvent.click(within(win).getByRole("tab", { name: "Ajustes" }));
    });
    await settle();
    expect(within(win).getByRole("tabpanel").scrollTop).toBe(0);
    within(win).getByRole("tabpanel").scrollTop = 500;
    await act(async () => {
      fireEvent.click(within(win).getByRole("tab", { name: "Feed" }));
    });
    await settle();
    expect(within(win).getByRole("tabpanel").scrollTop).toBe(0);
    // O Mudar horário: os Ajustes abrem com o campo do resumo em foco (não no topo).
    const scrolled: Element[] = [];
    const original = Element.prototype.scrollIntoView;
    Element.prototype.scrollIntoView = function (this: Element) {
      scrolled.push(this);
    };
    try {
      await act(async () => {
        fireEvent.click(within(win.querySelector('[data-nanty-planned="digest"]') as HTMLElement).getByRole("button", { name: "Mudar horário" }));
      });
      await settle();
    } finally {
      Element.prototype.scrollIntoView = original;
    }
    expect(document.activeElement?.hasAttribute("data-nanty-digest-time")).toBe(true);
    expect(scrolled.some((el) => el.hasAttribute("data-nanty-digest-time"))).toBe(true);
  });
});

describe("textos", () => {
  it("em inglês, nada de he, him ou his para o Nanty", async () => {
    const { en } = await import("../../i18n/en");
    const pronouns = Object.entries(en).filter(([key, text]) => (key.startsWith("nanty.") || key.startsWith("settings.agents.nanty.")) && /\b(he|him|his|himself)\b/i.test(text));
    expect(pronouns).toEqual([]);
  });
});

describe("Configurações > Agentes", () => {
  it('a linha do Nanty com o modelo atual; "Abrir o Nanty" abre a janela nos Ajustes', async () => {
    const view = await renderShell({ ...SPACE, nanty: { config: { ...NANTY_CONFIG, connection_id: "gem", model: "gemini-flash" }, telegram: NANTY_TELEGRAM } });
    view.server.reply("GET", /^\/api\/config\/connections\/[^/]+\/models/, () => ({ status: 200, body: { models: [], truncated: false } }));
    view.server.reply("GET", /^\/api\/system\/prereqs$/, () => ({ status: 200, body: [] }));
    await act(async () => activateApp("Configurações"));
    await settle();
    const settings = screen.getByRole("region", { name: "Configurações" });
    await act(async () => {
      fireEvent.click(within(settings).getByRole("tab", { name: /^Agentes/ }));
    });
    await settle();
    const row = settings.querySelector<HTMLElement>("[data-nanty-agent]") as HTMLElement;
    expect(row.querySelector("[data-nanty-agent-model]")?.textContent).toBe("Modelo gemini-flash, pela conexão Gemini pessoal.");
    // Fora da janela do Nanty, o ícone de linha: o avatar colorido fica lá dentro.
    expect(row.querySelector('svg[data-icon="nanty"]')).not.toBeNull();
    expect(row.querySelector("svg[data-nanty-avatar]")).toBeNull();
    // Sem duplicar os campos: nenhum campo na linha.
    expect(row.querySelectorAll("input, select")).toHaveLength(0);
    await act(async () => {
      fireEvent.click(within(row).getByRole("button", { name: "Abrir o Nanty" }));
    });
    await settle();
    const win = nantyWindow();
    expect(win.hidden).toBe(false);
    expect(within(win).getByRole("tab", { name: "Ajustes" }).getAttribute("aria-selected")).toBe("true");
    expect(win.querySelector("[data-nanty-settings]")).not.toBeNull();
  });
});
