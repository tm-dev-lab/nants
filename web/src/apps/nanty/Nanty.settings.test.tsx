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

async function open(tab: "Regras" | "Ajustes", extra: RenderShellOptions["nanty"] = {}) {
  const view = await renderShell({ ...SPACE, nanty: { config: NANTY_CONFIG, telegram: NANTY_TELEGRAM, ...extra } });
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
    expect(within(dev).getByRole("textbox", { name: "Modelo de reserva do papel Desenvolvedor" }).getAttribute("placeholder")).toBe("Modelo");
    await act(async () => {
      fireEvent.change(within(dev).getByRole("combobox", { name: "Conexão de reserva do papel Desenvolvedor" }), { target: { value: "oai" } });
    });
    await act(async () => {
      fireEvent.change(within(dev).getByRole("textbox", { name: "Modelo de reserva do papel Desenvolvedor" }), { target: { value: "gpt-x" } });
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

  it("número fora da faixa trava o salvar com a dica; a ordem da voz sobe, desce e sai", async () => {
    const { win } = await open("Ajustes", { config: { ...NANTY_CONFIG, voice: { ...NANTY_CONFIG.voice, enabled: true, order: [{ connection_id: "gem", model: "", voice: "" }] } } });
    const panel = win.querySelector<HTMLElement>("[data-nanty-settings]") as HTMLElement;
    await act(async () => {
      fireEvent.change(within(panel).getByLabelText("Orçamento em US$"), { target: { value: "5000" } });
    });
    expect(panel.textContent).toContain("Use um número de 0 a 1.000.");
    expect((panel.querySelector("[data-nanty-save]") as HTMLButtonElement).disabled).toBe(true);
    const order = panel.querySelector<HTMLElement>("[data-nanty-voice-order]") as HTMLElement;
    expect([...order.querySelectorAll("[data-nanty-voice-step]")].map((s) => s.getAttribute("data-nanty-voice-step"))).toEqual(["gem", "text"]);
    await act(async () => {
      fireEvent.change(within(panel).getByLabelText("Adicionar uma conexão à ordem"), { target: { value: "oai" } });
    });
    await act(async () => {
      fireEvent.click(within(order).getByRole("button", { name: "Subir OpenAI direto" }));
    });
    expect([...order.querySelectorAll("[data-nanty-voice-step]")].map((s) => s.getAttribute("data-nanty-voice-step"))).toEqual(["oai", "gem", "text"]);
    await act(async () => {
      fireEvent.click(within(order).getByRole("button", { name: "Tirar Gemini pessoal da ordem" }));
    });
    expect([...order.querySelectorAll("[data-nanty-voice-step]")].map((s) => s.getAttribute("data-nanty-voice-step"))).toEqual(["oai", "text"]);
  });

  it("a voz oferece só as conexões que servem; a incompatível já gravada aparece com aviso", async () => {
    const plan: ConnectionView = { id: "plan", name: "Plano ChatGPT", base_url: "https://chatgpt.com/backend-api/codex", api_key_masked: "", kind: "chatgpt_plan" };
    const claude: ConnectionView = { id: "claude", name: "Anthropic", base_url: "https://api.anthropic.com", api_key_masked: "********1234", kind: "anthropic" };
    const voice = { ...NANTY_CONFIG.voice, enabled: true, order: [{ connection_id: "plan", model: "", voice: "" }], transcribe: { connection_id: "oai", model: "" } };
    const view = await renderShell({ ...SPACE, connections: [...CONNS, plan, claude], nanty: { config: { ...NANTY_CONFIG, voice }, telegram: NANTY_TELEGRAM } });
    await act(async () => activateApp("Nanty"));
    await settle();
    const win = nantyWindow();
    await act(async () => {
      fireEvent.click(within(win).getByRole("tab", { name: "Ajustes" }));
    });
    await settle();
    const card = win.querySelector<HTMLElement>('[data-nanty-pref="voice"]') as HTMLElement;
    const options = (label: string) => [...(within(card).getByLabelText(label) as HTMLSelectElement).options].map((o) => o.value);
    // A ordem: Gemini e por chave; nem o plano nem a Anthropic.
    expect(options("Adicionar uma conexão à ordem")).toEqual(["", "gem", "oai"]);
    // A transcrição: só o Gemini (e a gravada, incompatível, para não sumir sem aviso).
    expect(options("Conexão que transcreve os seus áudios")).toEqual(["", "gem", "oai"]);
    expect(card.querySelector('[data-nanty-voice-warn="transcribe"]')?.textContent).toBe("A conexão OpenAI direto não transcreve áudio. Escolha uma do Google Gemini.");
    expect(card.querySelector('[data-nanty-voice-warn="plan"]')?.textContent).toBe("Plano ChatGPT não serve para a voz. Escolha uma conexão do Google Gemini ou uma por chave de API.");
    // Escolher o Gemini tira o aviso.
    await act(async () => {
      fireEvent.change(within(card).getByLabelText("Conexão que transcreve os seus áudios"), { target: { value: "gem" } });
    });
    expect(card.querySelector('[data-nanty-voice-warn="transcribe"]')).toBeNull();
    expect(options("Conexão que transcreve os seus áudios")).toEqual(["", "gem"]);
    expect(view.server.count("PUT", /^\/api\/nanty\/config$/)).toBe(0);
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
    // O idioma do Nanty: o da interface.
    expect(win.querySelector('[data-nanty-pref="lang"]')?.textContent).toContain("O Nanty fala em Português (Brasil)");
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
    const config = { ...NANTY_CONFIG, lang: "en", voice: { ...NANTY_CONFIG.voice, order: [{ connection_id: "gem", model: "", voice: "" }] } };
    const { win } = await open("Settings" as "Ajustes", { config, telegram, status: { ...NANTY_STATUS, budget } });
    const leftovers = (root: HTMLElement) => portugueseLeftovers(root).filter((text) => !/^(Loja|Blog|Gemini pessoal|OpenAI direto)$/.test(text));
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
