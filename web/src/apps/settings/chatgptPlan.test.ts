/*
 * Regras da conexão pelo plano ChatGPT (spec 116 T03): o retorno do login, os motivos de erro (a
 * mesma lista do servidor, traduzida nos dois idiomas), quem usa o plano e a seção das boas-vindas.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import type { Agents, ConnectionView } from "../../api/types";
import { setLanguage } from "../../i18n";
import {
  agentsUsePlan,
  captureChatgptReturn,
  dismissChatgptReturn,
  dismissChatgptWelcome,
  LOGIN_ERROR_REASONS,
  loginErrorText,
  parseChatgptReturn,
  parseChatgptSection,
  planConnectionToReconnect,
  planEmail,
  planState,
  planStateText,
  rememberReturnSpace,
  resetChatgptReturn,
  RETURN_SPACE_KEY,
} from "./chatgptPlan";

afterEach(() => {
  resetChatgptReturn();
  window.sessionStorage.clear();
  setLanguage("pt-BR", { persist: false });
});

const KEY: ConnectionView = { id: "openai", name: "OpenAI", base_url: "https://api.openai.com/v1", api_key_masked: "****" };
const PLAN: ConnectionView = { id: "chatgpt", name: "ChatGPT (ana@x.com)", base_url: "", api_key_masked: "", kind: "chatgpt_plan", chatgpt: { state: "connected", email: "ana@x.com" } };
const PLAN_OUT: ConnectionView = { ...PLAN, id: "chatgpt-2", chatgpt: { state: "needs_login", email: "bia@x.com" } };

function agents(architect: string, developer = architect, reviewer = architect): Agents {
  const one = (id: string) => ({ connection_id: id, model: "m", effort: "low" as const });
  return { architect: one(architect), developer: one(developer), reviewer: one(reviewer) };
}

describe("retorno do login", () => {
  it("lê o resultado da URL e recusa valores fora do formato", () => {
    expect(parseChatgptReturn("?chatgpt=connected&connection=chatgpt")).toEqual({ kind: "connected", connectionId: "chatgpt" });
    expect(parseChatgptReturn("?chatgpt=error&reason=access_denied")).toEqual({ kind: "error", reason: "access_denied" });
    expect(parseChatgptReturn("?chatgpt=error&reason=<script>")).toEqual({ kind: "error", reason: "unknown" });
    expect(parseChatgptReturn("?chatgpt=connected&connection=../x")).toBeNull();
    expect(parseChatgptReturn("?space=p1")).toBeNull();
  });

  it("guarda o resultado e volta ao espaço em que o login começou (uma vez)", () => {
    rememberReturnSpace("p1");
    expect(window.sessionStorage.getItem(RETURN_SPACE_KEY)).toBe("p1");
    expect(captureChatgptReturn("?chatgpt=connected&connection=chatgpt")).toBe("/?space=p1");
    expect(window.sessionStorage.getItem(RETURN_SPACE_KEY)).toBeNull();
    // Sem espaço guardado (outra origem ou aba nova): as boas-vindas.
    expect(captureChatgptReturn("?chatgpt=error&reason=scope_missing")).toBe("/");
    // Sem retorno de login, nada muda.
    expect(captureChatgptReturn("?space=p2")).toBeNull();
    rememberReturnSpace(null);
    expect(window.sessionStorage.getItem(RETURN_SPACE_KEY)).toBeNull();
  });

  it("dispensar o resultado e as boas-vindas não falha sem nada guardado", () => {
    dismissChatgptReturn();
    dismissChatgptWelcome();
    expect(captureChatgptReturn("?chatgpt=bogus")).toBe("/");
  });
});

describe("motivos de erro do login", () => {
  it("cobrem todos os motivos que o servidor manda no retorno", () => {
    const source = readFileSync(resolve(process.cwd(), "../src/server/chatgpt/mod.rs"), "utf8");
    const server = [...source.matchAll(/LoginFailure::\w+ => "([a-z_]+)"/g)].map((m) => m[1]);
    expect(server.length).toBeGreaterThan(5);
    expect([...LOGIN_ERROR_REASONS].sort()).toEqual([...server].sort());
  });

  it("têm texto próprio nos dois idiomas; motivo desconhecido cai no texto geral", () => {
    for (const lang of ["pt-BR", "en"] as const) {
      setLanguage(lang, { persist: false });
      const texts = LOGIN_ERROR_REASONS.map(loginErrorText);
      expect(new Set(texts).size).toBe(texts.length);
      expect(loginErrorText("unknown")).not.toBe(texts[0]);
    }
    expect(loginErrorText("access_denied")).toBe("Access was not allowed on the ChatGPT account.");
    setLanguage("pt-BR", { persist: false });
    expect(loginErrorText("access_denied")).toBe("O acesso não foi permitido na conta ChatGPT.");
    expect(loginErrorText("__proto__")).toBe("Tente de novo.");
  });
});

describe("conexões pelo plano", () => {
  it("estado, e-mail e texto do estado", () => {
    expect(planState(PLAN)).toBe("connected");
    expect(planState({ ...PLAN, chatgpt: undefined } as ConnectionView)).toBe("disconnected");
    expect(planEmail(PLAN_OUT)).toBe("bia@x.com");
    expect(planEmail({ ...PLAN, chatgpt: { state: "disconnected" } })).toBe("");
    expect(planStateText("connected")).toBe("conectada");
    expect(planStateText("needs_login")).toBe("precisa entrar de novo");
    expect(planStateText("disconnected")).toBe("sem login");
  });

  it("algum papel usa o plano; qual conexão reconectar", () => {
    expect(agentsUsePlan(agents("openai"), [KEY, PLAN])).toBe(false);
    expect(agentsUsePlan(agents("openai", "chatgpt"), [KEY, PLAN])).toBe(true);
    expect(agentsUsePlan(null, [PLAN])).toBe(false);
    expect(planConnectionToReconnect(agents("chatgpt", "chatgpt-2"), [KEY, PLAN, PLAN_OUT])?.id).toBe("chatgpt-2");
    expect(planConnectionToReconnect(agents("chatgpt", "openai"), [KEY, PLAN, PLAN_OUT])?.id).toBe("chatgpt");
    expect(planConnectionToReconnect(agents("openai"), [KEY, PLAN])).toBeNull();
  });

  it("seção das boas-vindas no ui-state", () => {
    expect(parseChatgptSection({ welcomed: true })).toEqual({ welcomed: true });
    expect(parseChatgptSection({ welcomed: "sim" })).toEqual({ welcomed: false });
    expect(parseChatgptSection(undefined)).toEqual({ welcomed: false });
  });
});
