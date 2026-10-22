import { describe, expect, it } from "vitest";
import {
  describeTestResult,
  MSG,
  validateApiKey,
  validateBaseUrl,
  validateConnectionId,
  validateConnectionForm,
  validateModel,
  validateName,
  WIRE_API_OPTIONS,
  wireApiCardLabel,
  wireApiOf,
  type ConnectionFormValues,
} from "./rules";

const base: ConnectionFormValues = {
  id: "openai-direct",
  name: "OpenAI direto",
  base_url: "https://api.openai.com/v1",
  api_key: "sk-abcdefghijk",
};

describe("validateConnectionId", () => {
  it("aceita [a-zA-Z0-9_-] de 1 a 64 caracteres", () => {
    expect(validateConnectionId("a")).toBeNull();
    expect(validateConnectionId("A_b-9")).toBeNull();
    expect(validateConnectionId("x".repeat(64))).toBeNull();
  });

  it("rejeita vazio, longo e caracteres fora do conjunto", () => {
    expect(validateConnectionId("")).toBe(MSG.idRequired);
    expect(validateConnectionId("x".repeat(65))).toBe(MSG.idInvalid);
    expect(validateConnectionId("a b")).toBe(MSG.idInvalid);
    expect(validateConnectionId("a/b")).toBe(MSG.idInvalid);
    expect(validateConnectionId("ção")).toBe(MSG.idInvalid);
  });
});

describe("validateBaseUrl", () => {
  it("aceita http e https com host", () => {
    expect(validateBaseUrl("https://api.openai.com/v1")).toBeNull();
    expect(validateBaseUrl("http://127.0.0.1:8080")).toBeNull();
  });

  it("rejeita esquemas não http, sem host e malformadas", () => {
    expect(validateBaseUrl("")).toBe(MSG.urlRequired);
    expect(validateBaseUrl("ftp://x.com")).toBe(MSG.urlInvalid);
    expect(validateBaseUrl("nao-e-url")).toBe(MSG.urlInvalid);
    expect(validateBaseUrl("https://")).toBe(MSG.urlInvalid);
  });

  it("rejeita userinfo, query e fragmento", () => {
    expect(validateBaseUrl("https://user:pass@x.com")).toBe(MSG.urlUserinfo);
    expect(validateBaseUrl("https://user@x.com")).toBe(MSG.urlUserinfo);
    expect(validateBaseUrl("https://x.com/v1?key=1")).toBe(MSG.urlQuery);
    expect(validateBaseUrl("https://x.com/v1#frag")).toBe(MSG.urlQuery);
    expect(validateBaseUrl("https://x.com/v1?")).toBe(MSG.urlQuery);
  });
});

describe("validateApiKey", () => {
  it("aceita chave literal e env:NOME válido", () => {
    expect(validateApiKey("sk-123")).toBeNull();
    expect(validateApiKey("env:OPENAI_API_KEY")).toBeNull();
  });

  it("rejeita vazia e env: inválido", () => {
    expect(validateApiKey("   ")).toBe(MSG.keyRequired);
    expect(validateApiKey("env:")).toBe(MSG.keyEnvInvalid);
    expect(validateApiKey("env:1ABC")).toBe(MSG.keyEnvInvalid);
    expect(validateApiKey("env:A B")).toBe(MSG.keyEnvInvalid);
  });
});

describe("validateModel/validateName", () => {

  it("model e name: obrigatórios, até 128, sem controle", () => {
    expect(validateModel("")).toBe(MSG.modelRequired);
    expect(validateModel("gpt-4o")).toBeNull();
    expect(validateModel("m".repeat(129))).toBe(MSG.modelTooLong);
    expect(validateModel("a\nb")).toBe(MSG.modelTooLong);
    expect(validateName("")).toBe(MSG.nameRequired);
    expect(validateName("n".repeat(129))).toBe(MSG.nameTooLong);
  });
});

describe("validateConnectionForm", () => {
  it("formulário válido não tem erros", () => {
    expect(validateConnectionForm(base, false)).toEqual({});
  });

  it("exige chave na criação, mas não na edição", () => {
    expect(validateConnectionForm({ ...base, api_key: "" }, false).api_key).toBe(MSG.keyRequired);
    expect(validateConnectionForm({ ...base, api_key: "" }, true)).toEqual({});
  });


  it("reporta todos os campos inválidos de uma vez", () => {
    const errors = validateConnectionForm(
      { ...base, id: "", name: "", base_url: "https://u:p@x.com", api_key: "env:" },
      false,
    );
    expect(Object.keys(errors).sort()).toEqual(["api_key", "base_url", "id", "name"]);
  });
});

describe("describeTestResult", () => {
  it("mostra latência no sucesso", () => {
    expect(describeTestResult({ ok: true, latency_ms: 42 })).toBe("Conexão ok (42 ms).");
  });

  it("mostra a contagem de modelos (singular e plural) com a latência", () => {
    expect(describeTestResult({ ok: true, latency_ms: 42, models_count: 3 })).toBe(
      "Conexão ok · 3 modelos (42 ms).",
    );
    expect(describeTestResult({ ok: true, latency_ms: 5, models_count: 1 })).toBe(
      "Conexão ok · 1 modelo (5 ms).",
    );
    expect(describeTestResult({ ok: true, models_count: 0 })).toBe("Conexão ok · 0 modelos.");
  });

  it("traduz cada código fixo e inclui o status HTTP", () => {
    expect(describeTestResult({ ok: false, error: "invalid_config" })).toContain("Configuração inválida");
    expect(describeTestResult({ ok: false, error: "timeout" })).toContain("Tempo esgotado");
    expect(describeTestResult({ ok: false, error: "connect_failed" })).toContain("Não foi possível conectar");
    expect(describeTestResult({ ok: false, error: "tls_failed" })).toContain("TLS");
    expect(describeTestResult({ ok: false, error: "bad_response" })).toContain("inesperada");
    expect(describeTestResult({ ok: false, error: "unsupported" })).toBe(
      "Este servidor não lista modelos; informe um modelo para testar por chat",
    );
    expect(describeTestResult({ ok: false, error: "http_error", status: 401 })).toBe(
      "O provedor respondeu com erro HTTP 401.",
    );
  });

  it("código desconhecido vira mensagem genérica (sem eco)", () => {
    expect(describeTestResult({ ok: false, error: "<script>x</script>" })).toBe("Falha no teste de conexão.");
  });
});

describe("controle C0/DEL/C1 e contagem por code point (espelha char::is_control e chars().count())", () => {
  it("rejeita C1 (U+0085 e U+009F) em nome e modelo", () => {
    expect(validateName("ab\u0085cd")).toBe(MSG.nameTooLong);
    expect(validateName("ab\u009fcd")).toBe(MSG.nameTooLong);
    expect(validateModel("gpt\u0085")).toBe(MSG.modelTooLong);
    expect(validateModel("gpt\u009f")).toBe(MSG.modelTooLong);
  });

  it("rejeita DEL e C0; aceita caracteres fora dessas faixas", () => {
    expect(validateModel("a\u007fb")).toBe(MSG.modelTooLong);
    expect(validateModel("a\u0000b")).toBe(MSG.modelTooLong);
    expect(validateModel("a b")).toBeNull();
    expect(validateName("São Paulo")).toBeNull();
  });

  it("limites contam code points: 65 emojis (130 unidades UTF-16) aceitos, 129 rejeitados", () => {
    const emoji = "\u{1F600}";
    expect(emoji.length).toBe(2);
    expect(validateModel(emoji.repeat(65))).toBeNull();
    expect(validateModel(emoji.repeat(129))).toBe(MSG.modelTooLong);
    expect(validateName(emoji.repeat(65))).toBeNull();
    expect(validateName(emoji.repeat(129))).toBe(MSG.nameTooLong);
  });

});

describe("protocolo da conexão (wire_api)", () => {
  it("wireApiOf: ausente ou desconhecido vira chat; só responses escolhe Responses", () => {
    expect(wireApiOf({ wire_api: "responses" })).toBe("responses");
    expect(wireApiOf({ wire_api: "chat" })).toBe("chat");
    expect(wireApiOf({})).toBe("chat");
    expect(wireApiOf({ wire_api: "outro" })).toBe("chat");
  });

  it("rótulos do cartão e opções na ordem chat, responses", () => {
    expect(wireApiCardLabel("chat")).toBe("Chat Completions");
    expect(wireApiCardLabel("responses")).toBe("Responses");
    expect(WIRE_API_OPTIONS.map((o) => o.value)).toEqual(["chat", "responses"]);
  });
});
