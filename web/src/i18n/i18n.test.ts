import { describe, expect, it } from "vitest";
import {
  DICTIONARIES,
  LANG_STORAGE_KEY,
  formatDate,
  formatDateIn,
  formatDateTime,
  formatNumber,
  formatNumberIn,
  formatTime,
  formatUsd,
  formatUsdIn,
  getLanguage,
  initLanguage,
  readStoredLanguage,
  setLanguage,
  subscribeLanguage,
  t,
  tp,
  translate,
  translatePlural,
} from "./index";
import { en } from "./en";
import { LANGS, detectLanguage, isLang, langFromTag, languageStateValue, parseLanguageState } from "./lang";
import { ptBR } from "./ptBR";

function memoryStorage(initial: Record<string, string> = {}) {
  const map = new Map(Object.entries(initial));
  return {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => {
      map.set(k, v);
    },
    map,
  };
}

describe("dicionários", () => {
  it("pt-BR e en têm exatamente as mesmas chaves", () => {
    expect(Object.keys(en).sort()).toEqual(Object.keys(ptBR).sort());
  });

  it("nenhum texto vazio", () => {
    for (const lang of LANGS) {
      for (const [key, value] of Object.entries(DICTIONARIES[lang])) {
        expect(value.trim().length, `${lang} ${key}`).toBeGreaterThan(0);
      }
    }
  });

  it("os mesmos parâmetros nos dois idiomas", () => {
    const params = (s: string) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
    for (const key of Object.keys(ptBR) as (keyof typeof ptBR)[]) {
      expect(params(en[key]), key).toEqual(params(ptBR[key]));
    }
  });

  it("sem travessão nem emoji nos dois idiomas", () => {
    const forbidden = /[–—]|\p{Extended_Pictographic}/u;
    for (const lang of LANGS) {
      for (const [key, value] of Object.entries(DICTIONARIES[lang])) {
        expect(forbidden.test(value), `${lang} ${key}: ${value}`).toBe(false);
      }
    }
  });

  it("plurais têm as duas formas", () => {
    for (const key of Object.keys(ptBR)) {
      if (key.endsWith(".one")) expect(Object.keys(ptBR)).toContain(`${key.slice(0, -4)}.other`);
    }
  });

  it("nomes dos idiomas sempre no próprio idioma", () => {
    for (const lang of LANGS) {
      expect(translate(lang, "language.name.pt-BR")).toBe("Português (Brasil)");
      expect(translate(lang, "language.name.en")).toBe("English");
    }
  });

  it("termos em inglês da decisão 7", () => {
    expect(translate("en", "role.architect")).toBe("Architect");
    expect(translate("en", "role.developer")).toBe("Developer");
    expect(translate("en", "role.reviewer")).toBe("Reviewer");
    expect(translate("en", "autoApprove.label")).toBe("Auto-approve");
    expect(translate("en", "autoApprove.short")).toBe("Auto");
  });
});

describe("detecção", () => {
  it("preferência salva vence o navegador", () => {
    expect(detectLanguage("pt-BR", ["en-US"])).toBe("pt-BR");
    expect(detectLanguage("en", ["pt-BR"])).toBe("en");
  });

  it("preferência inválida é ignorada", () => {
    expect(detectLanguage("fr", ["pt-BR"])).toBe("pt-BR");
    expect(detectLanguage(null, ["pt-BR"])).toBe("pt-BR");
  });

  it('["pt-PT"] -> pt-BR', () => {
    expect(detectLanguage(null, ["pt-PT"])).toBe("pt-BR");
  });

  it('["en-GB","pt-BR"] -> en (vale a ordem)', () => {
    expect(detectLanguage(null, ["en-GB", "pt-BR"])).toBe("en");
  });

  it('["es","pt"] -> pt-BR (pula o que não casa)', () => {
    expect(detectLanguage(null, ["es", "pt"])).toBe("pt-BR");
  });

  it("vazio ou sem casamento -> en", () => {
    expect(detectLanguage(null, [])).toBe("en");
    expect(detectLanguage(null, undefined)).toBe("en");
    expect(detectLanguage(null, ["es", "fr-FR"])).toBe("en");
  });

  it("só o idioma principal decide", () => {
    expect(langFromTag("PT_br")).toBe("pt-BR");
    expect(langFromTag("en")).toBe("en");
    expect(langFromTag("ptx")).toBeNull();
    expect(langFromTag("eng")).toBeNull();
  });

  it("seção language do estado da interface", () => {
    expect(parseLanguageState({ lang: "en" })).toBe("en");
    expect(parseLanguageState({ lang: "pt-BR" })).toBe("pt-BR");
    expect(parseLanguageState({ lang: "es" })).toBeNull();
    expect(parseLanguageState("en")).toBeNull();
    expect(parseLanguageState(undefined)).toBeNull();
    expect(parseLanguageState(languageStateValue("en"))).toBe("en");
    expect(isLang("pt-BR")).toBe(true);
    expect(isLang("pt")).toBe(false);
  });
});

describe("idioma ativo", () => {
  it("o ambiente de teste fixa pt-BR (C-05)", () => {
    expect(getLanguage()).toBe("pt-BR");
    expect(document.documentElement.lang).toBe("pt-BR");
    expect(t("window.conversation")).toBe("Conversa");
  });

  it("troca avisa os assinantes, aplica <html lang> e grava o espelho", () => {
    const storage = memoryStorage();
    const seen: string[] = [];
    const off = subscribeLanguage(() => seen.push(getLanguage()));
    setLanguage("en", { storage });
    off();
    expect(seen).toEqual(["en"]);
    expect(document.documentElement.lang).toBe("en");
    expect(storage.map.get(LANG_STORAGE_KEY)).toBe("en");
    expect(t("window.conversation")).toBe("Conversation");
  });

  it("mesmo idioma não avisa de novo", () => {
    let calls = 0;
    const off = subscribeLanguage(() => {
      calls += 1;
    });
    setLanguage("pt-BR", { persist: false });
    off();
    expect(calls).toBe(0);
  });

  it("initLanguage usa o espelho local e depois o navegador", () => {
    expect(initLanguage(memoryStorage({ [LANG_STORAGE_KEY]: "en" }), ["pt-BR"])).toBe("en");
    expect(document.documentElement.lang).toBe("en");
    expect(initLanguage(memoryStorage(), ["pt-PT"])).toBe("pt-BR");
    expect(document.documentElement.lang).toBe("pt-BR");
    expect(initLanguage(null, ["es"])).toBe("en");
  });

  it("readStoredLanguage tolera armazenamento quebrado", () => {
    const broken = {
      getItem: () => {
        throw new Error("bloqueado");
      },
      setItem: () => undefined,
    };
    expect(readStoredLanguage(broken)).toBeNull();
    expect(readStoredLanguage(null)).toBeNull();
  });
});

describe("tradução", () => {
  it("parâmetros nomeados", () => {
    expect(translate("pt-BR", "launcher.searchPlaceholder", { name: "Nants" })).toBe("Buscar em Nants");
    expect(translate("en", "launcher.searchPlaceholder", { name: "Nants" })).toBe("Search Nants");
  });

  it("parâmetro ausente fica visível", () => {
    expect(translate("en", "launcher.searchPlaceholder")).toBe("Search {name}");
  });

  it("plural por Intl.PluralRules", () => {
    // Decisão do Arquiteto (101/T01): em pt-BR o zero usa o plural, embora o CLDR o trate como singular.
    expect(translatePlural("pt-BR", "notifications.count", 0)).toBe("0 avisos");
    expect(translatePlural("pt-BR", "notifications.count", 1)).toBe("1 aviso");
    expect(translatePlural("pt-BR", "notifications.count", 2)).toBe("2 avisos");
    expect(translatePlural("en", "notifications.count", 0)).toBe("0 notices");
    expect(translatePlural("en", "notifications.count", 1)).toBe("1 notice");
    expect(translatePlural("en", "notifications.count", 1500)).toBe("1,500 notices");
    expect(translatePlural("pt-BR", "notifications.count", 1500)).toBe("1.500 avisos");
    expect(translatePlural("pt-BR", "notifications.count", 1_000_000)).toBe("1.000.000 avisos");
  });

  it("tp segue o idioma ativo", () => {
    expect(tp("notifications.count", 3)).toBe("3 avisos");
    setLanguage("en", { persist: false });
    expect(tp("notifications.count", 3)).toBe("3 notices");
  });
});

describe("formatadores Intl", () => {
  it("números no idioma", () => {
    expect(formatNumberIn("pt-BR", 1234.5)).toBe("1.234,5");
    expect(formatNumberIn("en", 1234.5)).toBe("1,234.5");
  });

  it("dólar sempre em US$", () => {
    expect(formatUsdIn("pt-BR", 1.5)).toBe("US$ 1,50");
    expect(formatUsdIn("en", 1.5)).toBe("US$ 1.50");
    expect(formatUsdIn("pt-BR", 0.0042)).toBe("US$ 0,0042");
    expect(formatUsdIn("en", 0)).toBe("US$ 0.00");
    expect(formatUsd(2)).toBe("US$ 2,00");
  });

  it("datas no idioma", () => {
    const date = new Date(2026, 9, 21, 14, 5);
    expect(formatDateIn("pt-BR", date)).toBe("21/10/2026");
    expect(formatDateIn("en", date)).toBe("10/21/26");
  });

  it("formatadores do idioma ativo acompanham a troca", () => {
    const date = new Date(2026, 9, 21, 14, 5);
    expect(formatDate(date)).toBe("21/10/2026");
    expect(formatTime(date)).toBe("14:05");
    expect(formatDateTime(date)).toBe("21/10/2026, 14:05");
    expect(formatNumber(0.5, { style: "percent" })).toBe("50%");
    setLanguage("en", { persist: false });
    expect(formatTime(date)).toBe("02:05 PM");
    expect(formatDateTime(date)).toBe("10/21/26, 2:05 PM");
    expect(formatNumber(1234.5)).toBe("1,234.5");
  });
});
