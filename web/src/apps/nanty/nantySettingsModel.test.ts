// @vitest-environment node
/*
 * Modelo puro dos Ajustes do Nanty (spec 138 T11): o pedido só com o que mudou, as faixas dos
 * números, o estado da voz e a leitura das respostas da API (a configuração, as vozes e o canal do
 * Telegram, com o link de pareamento só na forma `t.me/<bot>?start=<código>`).
 */
import { describe, expect, it } from "vitest";
import { NANTY_CONFIG } from "../../desktop/shell-test-utils";
import { parseNantySettings, parseNantyTelegram, parseNantyVoices, type NantySettings } from "./nantyApi";
import { budgetFraction, draftOf, invalidNumbers, keyRefused, minutesText, settingsPatch, voiceCapable, voiceCheckOf, voiceState, voiceWarning } from "./nantySettingsModel";

function saved(): NantySettings {
  const s = parseNantySettings(NANTY_CONFIG);
  if (s === null) throw new Error("configuração inválida");
  return s;
}

describe("pedido dos Ajustes", () => {
  it("sem mudança, vazio; cada campo entra só quando muda", () => {
    const s = saved();
    expect(settingsPatch(s, draftOf(s))).toEqual({});
    const d = { ...draftOf(s), quiet_end: "06:30", daily_budget_tokens: "300000", max_input_secs: "120", visible_hold_secs: "60" };
    expect(settingsPatch(s, d)).toEqual({ quiet_end: "06:30", daily_budget_tokens: 300000, voice: { max_input_secs: 120 }, telegram: { visible_hold_secs: 60 } });
    // Mudar e voltar ao valor salvo não manda nada.
    expect(settingsPatch(s, { ...draftOf(s), daily_budget_usd: "0,50" })).toEqual({});
    expect(settingsPatch(s, { ...draftOf(s), model: "  " })).toEqual({});
  });

  it("os projetos excluídos e a voz, na forma nova e sem espaços", () => {
    const s = saved();
    const d = { ...draftOf(s), excluded: ["p3", "p1"], voice_connection_id: "gem", speech_model: " m ", voice_name: "Puck", transcribe_model: "t" };
    expect(settingsPatch(s, d)).toEqual({
      excluded_projects: ["p1", "p3"],
      voice: { connection_id: "gem", speech_model: "m", voice: "Puck", transcribe_model: "t" },
    });
    // Vazio volta ao inicial no servidor: o pedido leva o vazio.
    expect(settingsPatch(s, { ...draftOf(s), voice_name: " " })).toEqual({ voice: { voice: "" } });
  });

  it("número fora da faixa do servidor: nenhum pedido", () => {
    const s = saved();
    for (const [field, value] of [
      ["daily_budget_usd", "-1"],
      ["daily_budget_usd", "1001"],
      ["daily_budget_tokens", "1.5"],
      ["max_input_secs", "2"],
      ["visible_hold_secs", "3601"],
      ["visible_hold_secs", ""],
    ] as const) {
      const d = { ...draftOf(s), [field]: value };
      expect(invalidNumbers(d)).toEqual([field]);
      expect(settingsPatch(s, d)).toBeNull();
    }
  });

  it("a barra e o relógio da validade", () => {
    expect([budgetFraction(1, 4), budgetFraction(9, 4), budgetFraction(0, 0), budgetFraction(1, 0)]).toEqual([0.25, 1, 0, 1]);
    expect([minutesText(600), minutesText(61), minutesText(-3)]).toEqual(["10:00", "1:01", "0:00"]);
  });
});

describe("conexões da voz (as regras do servidor)", () => {
  it("só o Gemini serve; o aviso da gravada", () => {
    expect([voiceCapable("google_gemini"), voiceCapable("api_key"), voiceCapable(undefined), voiceCapable("anthropic"), voiceCapable("chatgpt_plan")]).toEqual([true, false, false, false, false]);
    const conns = [
      { id: "g", name: "G", base_url: "", api_key_masked: "", kind: "google_gemini" as const },
      { id: "k", name: "K", base_url: "", api_key_masked: "" },
    ];
    expect([voiceWarning("", conns), voiceWarning("g", conns), voiceWarning("k", conns), voiceWarning("x", conns)]).toEqual([null, null, "unusable", "gone"]);
  });

  it("o estado é o que a rota de vozes responde", () => {
    const failure = (code: string | undefined, params?: Record<string, string>) => ({ ok: false as const, error: { kind: "http" as const, status: 409, message: "x", code, params } });
    const ready = voiceCheckOf({ ok: true, data: { connection_id: "g", voices: [], speech_models: [] } });
    expect([ready.status, voiceState(ready)]).toEqual(["ready", "ready"]);
    expect(voiceState(voiceCheckOf(failure("nanty_voice_no_key")))).toBe("no_key");
    expect(voiceState(voiceCheckOf(failure("nanty_voice_no_connection")))).toBe("no_connection");
    // O provedor falhou: a regra do servidor continua valendo, com a conexão que ele resolveu.
    const listFailed = voiceCheckOf(failure("nanty_voice_list_failed", { reason: "provider_error", connection_id: "g" }));
    expect(listFailed).toMatchObject({ status: "failed", connection_id: "g" });
    expect(voiceState(listFailed)).toBe("ready");
    expect(keyRefused(listFailed)).toBe(false);
    // A chave recusada não está pronta: repetir não resolve.
    const refused = voiceCheckOf(failure("nanty_voice_list_failed", { reason: "auth", connection_id: "g" }));
    expect(refused).toMatchObject({ status: "failed", connection_id: "g", reason: "auth" });
    expect([keyRefused(refused), voiceState(refused)]).toEqual([true, "key_refused"]);
    // O motivo só vale no código da lista.
    expect(voiceState(voiceCheckOf(failure("outro_codigo", { reason: "auth", connection_id: "g" })))).toBeNull();
    // Sem resposta do servidor: nada a afirmar.
    expect(voiceState(voiceCheckOf(failure(undefined)))).toBeNull();
    expect(voiceState({ status: "loading" })).toBeNull();
  });
});

describe("leitura da API", () => {
  it("as vozes: sem id fica de fora, sem repetir, e o rótulo só leva a descrição curta", () => {
    const voices = parseNantyVoices({
      connection_id: "g",
      voices: [
        { id: "Kore", display_name: "Kore", language_code: "pt-BR", description: "Firm" },
        { id: "Kore", display_name: "Kore de novo" },
        { display_name: "Sem id" },
        { id: "Puck", display_name: "", description: "x".repeat(41) },
      ],
      speech_models: ["gemini-3.8-flash-tts", 7, ""],
    });
    expect(voices).toEqual({ connection_id: "g", voices: [{ id: "Kore", name: "Kore (pt-BR, Firm)" }, { id: "Puck", name: "Puck" }], speech_models: [{ id: "gemini-3.8-flash-tts" }] });
    expect(parseNantyVoices({ connection_id: "g", voices: [] })?.speech_models).toEqual([]);
    expect(parseNantyVoices("x")).toBeNull();
  });

  it("a configuração: hora inválida descarta; a reserva vazia some; sem token na forma da tela", () => {
    expect(parseNantySettings({ ...NANTY_CONFIG, digest_time: "8h" })).toBeNull();
    const s = parseNantySettings({ ...NANTY_CONFIG, now: "25:99x", reserve: { developer: { connection_id: "oai", model: "m" }, reviewer: { connection_id: "", model: "" }, chefe: { connection_id: "x", model: "y" } } });
    expect(s?.now).toBeNull();
    expect(s?.reserve).toEqual({ developer: { connection_id: "oai", model: "m" } });
    expect(Object.keys(s?.telegram ?? {})).toEqual(["paired", "hold_while_visible", "visible_hold_secs"]);
  });

  it("o canal: o link só na forma do Telegram e com o próprio código; código fora da forma fecha a janela", () => {
    const code = "AbCdEfGh12345_-z";
    const base = { health: "connected", bot_username: "NantsNantyBot", bot_token_masked: "********oQ7z", configured: true, paired: false };
    const open = parseNantyTelegram({ ...base, pairing: { state: "open", code, link: `https://t.me/NantsNantyBot?start=${code}`, seconds_to_next: 30, expires_in: 300 } });
    expect(open?.pairing).toEqual({ state: "open", code, link: `https://t.me/NantsNantyBot?start=${code}`, seconds_to_next: 30, expires_in: 300 });
    for (const link of [`https://t.me/Outro?start=AAAAAAAAAAAAAAAA`, `http://t.me/NantsNantyBot?start=${code}`, `https://t.me/a/b?start=${code}`, "javascript:alert(1)"]) {
      expect(parseNantyTelegram({ ...base, pairing: { state: "open", code, link } })?.pairing.link).toBeNull();
    }
    expect(parseNantyTelegram({ ...base, pairing: { state: "open", code: "curto" } })?.pairing.state).toBe("closed");
    expect(parseNantyTelegram({ ...base, health: "sei-la" })).toBeNull();
    expect(parseNantyTelegram({ ...base, bot_username: "<script>", pairing: { state: "locked" } })).toMatchObject({ bot_username: null, pairing: { state: "locked", code: null } });
  });
});
