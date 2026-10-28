/*
 * Spec 133 T02: regras puras da conexão por papel (conexões prontas, página do provedor pelo tipo
 * da conexão, a troca de um papel só).
 */
import { afterEach, describe, expect, it } from "vitest";
import type { Agents, ConnectionView } from "../../api/types";
import { setLanguage } from "../../i18n";
import { PLAN_USAGE_URL } from "../settings/chatgptPlan";
import { connectionKindLabel, providerPage, readyConnections, roleOf, suggestedModel, withRoleConnection } from "./roleConnection";

afterEach(() => setLanguage("pt-BR", { persist: false }));

const conn = (over: Partial<ConnectionView>): ConnectionView => ({ id: "x", name: "X", base_url: "https://exemplo.com/v1", api_key_masked: "****", ...over });
const AGENTS: Agents = {
  architect: { connection_id: "plan", model: "sol", effort: "low" },
  developer: { connection_id: "ant", model: "haiku", effort: "low" },
  reviewer: { connection_id: "gem", model: "flash", effort: "medium", max_steps: 40 },
};

describe("conexão por papel", () => {
  it("prontas: o plano com login e as outras com chave", () => {
    const list = [
      conn({ id: "plan", kind: "chatgpt_plan", api_key_masked: "", chatgpt: { state: "connected", email: "a@x" } }),
      conn({ id: "plan-out", kind: "chatgpt_plan", api_key_masked: "", chatgpt: { state: "needs_login", email: "a@x" } }),
      conn({ id: "ant", kind: "anthropic" }),
      conn({ id: "vazia", api_key_masked: "" }),
    ];
    expect(readyConnections(list).map((c) => c.id)).toEqual(["plan", "ant"]);
  });

  it("página do provedor pelo tipo da conexão, só quando há uma oficial", () => {
    expect(providerPage(null, "anthropic", "billing")?.url).toBe("https://console.anthropic.com/settings/billing");
    expect(providerPage(null, "anthropic", "limits")?.url).toBe("https://console.anthropic.com/settings/limits");
    expect(providerPage(null, "google_gemini", "billing")?.url).toBe("https://aistudio.google.com/usage");
    expect(providerPage(null, "chatgpt_plan", "billing")).toEqual({ url: PLAN_USAGE_URL, label: "Gerenciar uso" });
    expect(providerPage(conn({ base_url: "https://api.openai.com/v1" }), "api_key", "billing")?.url).toContain("platform.openai.com");
    expect(providerPage(conn({ base_url: "https://openrouter.ai/api/v1" }), "api_key", "limits")).toBeNull();
    expect(providerPage(null, undefined, "limits")).toBeNull();
    // A conexão salva vale mais que o tipo gravado na mensagem.
    expect(providerPage(conn({ kind: "anthropic" }), "google_gemini", "billing")?.url).toContain("anthropic.com");
  });

  it("a troca muda só o papel pedido, com o modelo sem espaços; o resto fica igual", () => {
    const next = withRoleConnection(AGENTS, "reviewer", "plan", " terra ");
    expect(next.reviewer).toEqual({ connection_id: "plan", model: "terra", effort: "medium", max_steps: 40 });
    expect(next.architect).toBe(AGENTS.architect);
    expect(next.developer).toBe(AGENTS.developer);
  });

  it("modelo sugerido: o que outro papel já usa na conexão", () => {
    expect(suggestedModel(AGENTS, "plan")).toBe("sol");
    expect(suggestedModel(AGENTS, "nova")).toBe("");
  });

  it("papel e tipo da conexão", () => {
    expect(roleOf("developer")).toBe("developer");
    expect(roleOf("system")).toBeNull();
    expect(roleOf(3)).toBeNull();
    expect(connectionKindLabel("chatgpt_plan")).toBe("Plano ChatGPT");
    expect(connectionKindLabel(undefined)).toBe("Chave de API");
    setLanguage("en", { persist: false });
    expect(connectionKindLabel("chatgpt_plan")).toBe("ChatGPT plan");
  });
});
