/*
 * Regras puras das Configurações (spec 093, T08): teclado das abas, o que falta por aba, o resumo
 * da janela e as regras do editor de preços.
 */
import { describe, expect, it } from "vitest";
import type { Readiness } from "../../desktop/api";
import { notReady, READY } from "../../desktop/shell-test-utils";
import {
  buildPricing,
  connectionDetail,
  connectionHost,
  connectionUsageText,
  connectionUsers,
  formatPrice,
  parsePrice,
  priceBadge,
  priceRowOf,
  SETTINGS_WINDOW_TABS,
  settingsSummary,
  tabByKey,
  tabMissing,
} from "./settingsModel";

const input = (readiness: Readiness | null) => ({ space: null, readiness, projectCount: 0 });

describe("abas", () => {
  it("ordem e teclado (circula; Home e End vão às pontas)", () => {
    expect(SETTINGS_WINDOW_TABS).toEqual(["connections", "agents", "pricing", "machine", "appearance", "projects"]);
    expect(tabByKey("connections", "ArrowRight")).toBe("agents");
    expect(tabByKey("connections", "ArrowUp")).toBe("projects");
    expect(tabByKey("projects", "ArrowDown")).toBe("connections");
    expect(tabByKey("machine", "Home")).toBe("connections");
    expect(tabByKey("machine", "End")).toBe("projects");
    expect(tabByKey("machine", "Enter")).toBeNull();
  });

  it("o que falta por aba, sem repetir e com os recomendados (preços)", () => {
    const readiness: Readiness = {
      ...notReady(),
      recommended: { ok: false, missing: [{ id: "pricing:model", label: "Preço de model", settings_tab: "pricing" }] },
    };
    expect(tabMissing(readiness, "connections").map((i) => i.id)).toEqual(["connection"]);
    expect(tabMissing(readiness, "machine").map((i) => i.id)).toEqual(["docker_daemon"]);
    expect(tabMissing(readiness, "pricing").map((i) => i.id)).toEqual(["pricing:model"]);
    expect(tabMissing(readiness, "appearance")).toEqual([]);
    expect(tabMissing(null, "connections")).toEqual([]);
  });

  it("resumo da janela", () => {
    expect(settingsSummary(input(null))).toEqual({});
    expect(settingsSummary(input(READY)).subtitle).toBe("tudo pronto");
    expect(settingsSummary(input(notReady())).subtitle).toBe("3 pendências no setup");
    expect(
      settingsSummary(input({ ...READY, recommended: { ok: false, missing: [{ id: "pricing:m", label: "m", settings_tab: "pricing" }] } })).subtitle,
    ).toBe("tudo pronto · preços a cadastrar");
  });
});

describe("preços", () => {
  it("formata e lê números em pt-BR", () => {
    expect(formatPrice(1.25)).toBe("1,25");
    expect(formatPrice(0.1 + 0.2)).toBe("0,3");
    expect(parsePrice("1,5")).toBe(1.5);
    expect(parsePrice("")).toBeNull();
    expect(parsePrice("-1")).toBeNull();
    expect(parsePrice("abc")).toBeNull();
  });

  it("selo: sem preço, estimado (com a data) e manual", () => {
    expect(priceBadge(undefined)).toEqual({ text: "sem preço", tone: "muted" });
    expect(priceBadge(priceRowOf({ input_per_mtok: 1, output_per_mtok: 2, source: "estimated", estimated_at: "2026-10-01" }))).toEqual({
      text: "estimado (2026-10-01)",
      tone: "amber",
    });
    expect(priceBadge(priceRowOf({ input_per_mtok: 1, output_per_mtok: 2 })).text).toBe("manual");
  });

  it("mapa a salvar: linhas vazias ficam de fora, imagem e cache opcionais, inválido aponta o modelo", () => {
    const rows = {
      a: { input: "1", output: "2", cached: "", image: "0,04", source: "manual" as const },
      b: { input: "", output: "", cached: "", image: "", source: "manual" as const },
      c: { input: "1", output: "2", cached: "0,5", image: "", source: "estimated" as const, estimatedAt: "2026-10-01" },
    };
    expect(buildPricing(["a", "b", "c"], rows)).toEqual({
      ok: true,
      pricing: {
        a: { input_per_mtok: 1, output_per_mtok: 2, image_per_unit: 0.04, source: "manual" },
        c: { input_per_mtok: 1, output_per_mtok: 2, cached_input_per_mtok: 0.5, source: "estimated", estimated_at: "2026-10-01" },
      },
    });
    expect(buildPricing(["a"], { a: { ...rows.a, image: "x" } })).toEqual({ ok: false, model: "a" });
  });
});

describe("conexões em lista (095/T06)", () => {
  const agents = {
    architect: { connection_id: "a", model: "m", effort: "low" as const },
    developer: { connection_id: "b", model: "m", effort: "low" as const },
    reviewer: { connection_id: "a", model: "m", effort: "medium" as const },
  };
  it("'usada por' na ordem dos papéis; sem papel, 'sem uso'", () => {
    expect(connectionUsers(agents, "a")).toEqual(["Arquiteto", "Revisor"]);
    expect(connectionUsers(agents, "b")).toEqual(["Desenvolvedor"]);
    expect(connectionUsers(agents, "c")).toEqual([]);
    expect(connectionUsers(null, "a")).toEqual([]);
    expect(connectionUsageText(["Arquiteto", "Revisor"])).toBe("usada por Arquiteto, Revisor");
    expect(connectionUsageText([])).toBe("sem uso");
  });

  it("detalhe: host, protocolo e só a máscara; URL inválida volta inteira", () => {
    expect(connectionHost("https://api.openai.com/v1")).toBe("api.openai.com");
    expect(connectionHost("http://localhost:8080/v1")).toBe("localhost:8080");
    expect(connectionHost("não é url")).toBe("não é url");
    expect(connectionDetail({ id: "x", name: "X", base_url: "https://az.example.com/v1", api_key_masked: "****9", wire_api: "responses" })).toBe(
      "az.example.com · Responses · chave ****9",
    );
  });
});
