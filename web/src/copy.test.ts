import { describe, expect, it } from "vitest";
import {
  isProviderFailureCode,
  PROVIDER_THROTTLE_HINT,
  providerFailureDetail,
  providerFailureMessage,
  providerFailureText,
} from "./copy";

const HINT = "O modelo pode exigir o protocolo Responses: altere em Conexões, Editar.";

describe("vocabulário de falha do provedor", () => {
  it.each([
    "provider_http_100",
    "provider_http_400",
    "provider_http_599",
    "provider_timeout",
    "provider_connect_failed",
    "provider_bad_response",
    "provider_invalid_config",
  ])("reconhece %s", (code) => {
    expect(isProviderFailureCode(code)).toBe(true);
  });

  it.each([
    "provider_http_99",
    "provider_http_600",
    "provider_http_4000",
    "provider_http_40",
    "provider_http_abc",
    "provider_http_400 ",
    "backend_error",
    "bad_response",
    "sk-SEGREDO-123",
    "",
  ])("rejeita texto fora do vocabulário: %j", (code) => {
    expect(isProviderFailureCode(code)).toBe(false);
    expect(providerFailureMessage(code, "chat")).toBeNull();
  });
});

describe("mensagens das falhas do provedor", () => {
  it("HTTP 4xx (fora 401/403/404/429) com conexão chat acrescenta a dica de Responses", () => {
    expect(providerFailureMessage("provider_http_400", "chat")).toBe(
      `O provedor recusou a requisição (HTTP 400). Verifique o modelo e o protocolo da conexão. ${HINT}`,
    );
    expect(providerFailureMessage("provider_http_422", "chat")).toContain(HINT);
  });

  it("HTTP 4xx com conexão responses não tem dica", () => {
    expect(providerFailureMessage("provider_http_400", "responses")).toBe(
      "O provedor recusou a requisição (HTTP 400). Verifique o modelo e o protocolo da conexão.",
    );
  });

  it("HTTP 4xx sem informação do protocolo não tem dica", () => {
    expect(providerFailureMessage("provider_http_400", null)).not.toContain(HINT);
  });

  it("401 e 403 recusam a chave, sem dica", () => {
    expect(providerFailureMessage("provider_http_401", "chat")).toBe(
      "O provedor recusou a chave de API (HTTP 401). Verifique a conexão.",
    );
    expect(providerFailureMessage("provider_http_403", "chat")).toBe(
      "O provedor recusou a chave de API (HTTP 403). Verifique a conexão.",
    );
  });

  it("404 aponta URL e modelo, sem dica", () => {
    expect(providerFailureMessage("provider_http_404", "chat")).toBe(
      "O provedor não encontrou o endereço ou o modelo (HTTP 404). Verifique a URL e o modelo.",
    );
  });

  it("429 diz que o Nants reduz o ritmo, sem dica de Responses (spec 089)", () => {
    expect(providerFailureMessage("provider_http_429", "chat")).toBe(
      "O provedor limitou as requisições (HTTP 429). O Nants já reduz o ritmo sozinho; continue em alguns minutos.",
    );
  });

  it("5xx do provedor é 'O provedor falhou', sem dica", () => {
    expect(providerFailureMessage("provider_http_503", "chat")).toBe(
      "O provedor falhou (HTTP 503). Tente novamente.",
    );
  });

  it.each([
    ["provider_timeout", "O provedor não respondeu a tempo."],
    ["provider_connect_failed", "Não foi possível conectar ao provedor. Verifique a URL da conexão."],
    ["provider_bad_response", "O provedor devolveu uma resposta inesperada."],
    ["provider_invalid_config", "A conexão está mal configurada (chave ou URL inválida)."],
  ])("%s tem mensagem fixa", (code, text) => {
    expect(providerFailureMessage(code, "chat")).toBe(text);
    expect(providerFailureMessage(code, "responses")).toBe(text);
  });
});

describe("motivo da falha do provedor (spec 088)", () => {
  it("código com detalhe: texto pt-BR com o detalhe entre parênteses antes do ponto final", () => {
    expect(providerFailureText("provider_bad_response (incompleta: content_filter)", null)).toBe(
      "O provedor devolveu uma resposta inesperada (incompleta: content_filter).",
    );
    expect(providerFailureText("provider_timeout (vazia)", null)).toBe("O provedor não respondeu a tempo (vazia).");
  });

  it("sem detalhe, a mensagem fica como antes", () => {
    expect(providerFailureText("provider_bad_response", null)).toBe("O provedor devolveu uma resposta inesperada.");
    expect(providerFailureText("provider_http_400", "chat")).toBe(providerFailureMessage("provider_http_400", "chat"));
  });

  it("dica de Responses fica depois da frase com o detalhe", () => {
    expect(providerFailureText("provider_http_400 (erro do provedor)", "chat")).toBe(
      `O provedor recusou a requisição (HTTP 400). Verifique o modelo e o protocolo da conexão (erro do provedor). ${HINT}`,
    );
  });

  it.each([
    "backend_error (vazia)",
    "provider_http_600 (vazia)",
    "bad_response (incompleta)",
    "sk-SEGREDO (vazia)",
    "texto livre qualquer",
  ])("código fora do vocabulário continua sem tradução: %j", (text) => {
    expect(providerFailureText(text, null)).toBeNull();
  });

  it.each([
    ["mais de 80 caracteres", `provider_bad_response (${"x".repeat(81)})`],
    ["quebra de linha", "provider_bad_response (incompleta\nsegredo)"],
    ["retorno de carro", "provider_bad_response (incompleta\rsegredo)"],
    ["vazio", "provider_bad_response ()"],
    ["só espaços", "provider_bad_response (   )"],
  ])("detalhe inválido (%s) é ignorado", (_label, text) => {
    expect(providerFailureText(text, null)).toBe("O provedor devolveu uma resposta inesperada.");
  });

  it("detalhe com exatamente 80 caracteres é aceito", () => {
    const detail = "x".repeat(80);
    expect(providerFailureDetail(detail)).toBe(detail);
    expect(providerFailureDetail(`${detail}x`)).toBeNull();
    expect(providerFailureDetail(42)).toBeNull();
    expect(providerFailureDetail(undefined)).toBeNull();
  });
});

describe("dica de limite do provedor (spec 089)", () => {
  const LIMIT = "O provedor está limitando as requisições. O Nants já reduz o ritmo sozinho; continue em alguns minutos.";
  const ADVICE = "O Nants já reduz o ritmo sozinho; continue em alguns minutos.";

  it("a dica exportada é o texto da spec", () => {
    expect(PROVIDER_THROTTLE_HINT).toBe(LIMIT);
  });

  it("mensagem de sistema provider_bad_response com detalhe de limite ganha a dica", () => {
    expect(providerFailureText("provider_bad_response (provedor no limite: no_capacity)", null)).toBe(
      `O provedor devolveu uma resposta inesperada (provedor no limite: no_capacity). ${LIMIT}`,
    );
    expect(providerFailureText("provider_bad_response (provedor no limite)", "chat")).toBe(
      `O provedor devolveu uma resposta inesperada (provedor no limite). ${LIMIT}`,
    );
  });

  it("mensagem de sistema 429 traz o conselho de espera uma vez só", () => {
    const text = providerFailureText("provider_http_429", null) ?? "";
    expect(text).toBe(`O provedor limitou as requisições (HTTP 429). ${ADVICE}`);
    expect(text.split(ADVICE)).toHaveLength(2);
    expect(text).not.toContain("Tente novamente em instantes");
    expect(providerFailureText("provider_http_429 (cota)", null)).toBe(
      `O provedor limitou as requisições (HTTP 429) (cota). ${ADVICE}`,
    );
  });

  it.each([
    "provider_bad_response",
    "provider_bad_response (incompleta: content_filter)",
    "provider_bad_response (sem texto nem ação)",
    "provider_bad_response (limite do provedor)",
    "provider_timeout (provedor no limite)",
    "provider_http_400",
    "provider_http_503",
    "provider_connect_failed",
    "provider_invalid_config",
  ])("demais mensagens não ganham a dica: %j", (text) => {
    const out = providerFailureText(text, "chat") ?? "";
    expect(out).not.toContain(ADVICE);
    expect(out).not.toContain(LIMIT);
  });
});
