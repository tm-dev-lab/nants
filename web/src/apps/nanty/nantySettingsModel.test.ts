// @vitest-environment node
/*
 * Modelo puro dos Ajustes do Nanty (spec 138 T11): o pedido só com o que mudou, as faixas dos
 * números, a ordem da voz e a leitura das respostas da API (a configuração e o canal do Telegram,
 * com o link de pareamento só na forma `t.me/<bot>?start=<código>`).
 */
import { describe, expect, it } from "vitest";
import { NANTY_CONFIG } from "../../desktop/shell-test-utils";
import { parseNantySettings, parseNantyTelegram, type NantySettings } from "./nantyApi";
import { budgetFraction, draftOf, invalidNumbers, minutesText, moveStep, settingsPatch, speaks, transcribes, voiceWarning } from "./nantySettingsModel";

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

  it("os projetos excluídos, a ordem da voz e a transcrição", () => {
    const s = saved();
    const d = {
      ...draftOf(s),
      excluded: ["p3", "p1"],
      voice_order: [{ connection_id: "gem", model: " m ", voice: "" }],
      transcribe: { connection_id: "gem", model: "" },
    };
    expect(settingsPatch(s, d)).toEqual({
      excluded_projects: ["p1", "p3"],
      voice: { order: [{ connection_id: "gem", model: "m", voice: "" }], transcribe: { connection_id: "gem", model: "" } },
    });
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

  it("subir, descer e as bordas da lista; a barra e o relógio da validade", () => {
    expect(moveStep(["a", "b", "c"], 2, -1)).toEqual(["a", "c", "b"]);
    expect(moveStep(["a", "b", "c"], 0, 1)).toEqual(["b", "a", "c"]);
    expect(moveStep(["a", "b"], 0, -1)).toEqual(["a", "b"]);
    expect(moveStep(["a", "b"], 1, 1)).toEqual(["a", "b"]);
    expect([budgetFraction(1, 4), budgetFraction(9, 4), budgetFraction(0, 0), budgetFraction(1, 0)]).toEqual([0.25, 1, 0, 1]);
    expect([minutesText(600), minutesText(61), minutesText(-3)]).toEqual(["10:00", "1:01", "0:00"]);
  });
});

describe("conexões da voz (as regras do servidor)", () => {
  it("fala: Gemini e por chave; transcrição: só Gemini; o aviso da gravada", () => {
    expect([speaks("google_gemini"), speaks("api_key"), speaks(undefined), speaks("chatgpt_plan"), speaks("anthropic")]).toEqual([true, true, true, false, false]);
    expect([transcribes("google_gemini"), transcribes("api_key"), transcribes(undefined), transcribes("anthropic")]).toEqual([true, false, false, false]);
    const conns = [
      { id: "g", name: "G", base_url: "", api_key_masked: "", kind: "google_gemini" as const },
      { id: "k", name: "K", base_url: "", api_key_masked: "" },
    ];
    expect([voiceWarning("", conns, transcribes), voiceWarning("g", conns, transcribes), voiceWarning("k", conns, transcribes), voiceWarning("x", conns, speaks)]).toEqual([null, null, "unusable", "gone"]);
  });
});

describe("leitura da API", () => {
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
