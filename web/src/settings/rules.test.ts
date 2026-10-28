import { describe, expect, it } from "vitest";
import { setLanguage } from "../i18n";
import {
  connectionKindOf,
  describeModelsError,
  describeTestResult,
  effortHint,
  isGeminiCompatUrl,
  isNativeKind,
  MSG,
  NATIVE_PROVIDERS,
  nativeConnectionId,
  suggestionCandidates,
  suggestRoleModels,
  validateNativeForm,
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

describe("teste da conexão pelo plano ChatGPT (spec 116 T02)", () => {
  it("os códigos do plano têm texto próprio", () => {
    expect(describeTestResult({ ok: false, error: "plan_limit" })).toBe("Limite de uso do plano ChatGPT atingido.");
    expect(describeTestResult({ ok: false, error: "plan_not_eligible" })).toBe("Esta conta ChatGPT não pode usar o plano pelo Nants.");
    expect(describeTestResult({ ok: false, error: "plan_needs_login" })).toBe("Entre de novo no plano ChatGPT para usar esta conexão.");
  });
});

describe("códigos do teste das conexões nativas (spec 132 T08)", () => {
  it("cada código tem texto próprio no teste e na listagem, sem cair no genérico", () => {
    const expected: Record<string, string> = {
      auth_invalid: "O provedor recusou a chave. Confira se ela foi copiada inteira.",
      forbidden: "A chave não tem permissão para este recurso no provedor.",
      model_not_found: "Este modelo não existe no provedor. Escolha um da lista.",
      rate_limited: "O provedor atingiu o limite de pedidos. Tente de novo em instantes.",
      spend_limit: "A conta atingiu o teto de gasto no provedor. Aumente o limite no console dele.",
      daily_quota: "A cota diária do provedor acabou. Tente de novo quando ela renovar.",
    };
    for (const [code, text] of Object.entries(expected)) {
      expect(describeTestResult({ ok: false, error: code })).toBe(text);
      expect(describeModelsError(code)).toBe(text);
    }
  });
});

describe("tipo de conexão ainda sem adaptador (spec 132 T01)", () => {
  it("o teste e a listagem de modelos mostram o texto próprio", () => {
    const text = "Este tipo de conexão ainda não pode ser testado nesta versão.";
    expect(describeTestResult({ ok: false, error: "connection_kind_not_supported_yet" })).toBe(text);
    expect(describeModelsError("connection_kind_not_supported_yet")).toBe(text);
  });
});

describe("conexões nativas (spec 132 T10)", () => {
  it("o tipo vindo do servidor: desconhecido ou ausente é por chave", () => {
    expect(connectionKindOf({ kind: "anthropic" })).toBe("anthropic");
    expect(connectionKindOf({ kind: "google_gemini" })).toBe("google_gemini");
    expect(connectionKindOf({ kind: "chatgpt_plan" })).toBe("chatgpt_plan");
    expect(connectionKindOf({})).toBe("api_key");
    expect(connectionKindOf({ kind: "azure" })).toBe("api_key");
    expect(isNativeKind("anthropic") && isNativeKind("google_gemini") && !isNativeKind("api_key")).toBe(true);
  });

  it("o id nasce do nome, sem acento, e evita os já usados", () => {
    expect(nativeConnectionId("Anthropic", "anthropic", [])).toBe("anthropic");
    expect(nativeConnectionId("Gemini da Ação", "google_gemini", [])).toBe("gemini-da-acao");
    expect(nativeConnectionId("  ", "google_gemini", [])).toBe("gemini");
    expect(nativeConnectionId("Anthropic", "anthropic", ["anthropic", "anthropic-2"])).toBe("anthropic-3");
    const long = nativeConnectionId("x".repeat(100), "anthropic", []);
    expect(long.length).toBeLessThanOrEqual(64);
    expect(validateConnectionId(long)).toBeNull();
  });

  it("valida só o nome, a chave e o endereço avançado opcional", () => {
    const ok = { name: "Anthropic", api_key: "sk-ant-1", base_url_override: "" };
    expect(validateNativeForm(ok, false)).toEqual({});
    expect(validateNativeForm({ ...ok, api_key: "" }, false)).toEqual({ api_key: MSG.keyRequired });
    expect(validateNativeForm({ ...ok, api_key: "" }, true)).toEqual({});
    expect(validateNativeForm({ ...ok, api_key: "env:ANTHROPIC_API_KEY" }, false)).toEqual({});
    expect(validateNativeForm({ ...ok, api_key: "env:1X" }, false)).toEqual({ api_key: MSG.keyEnvInvalid });
    expect(validateNativeForm({ ...ok, base_url_override: "https://proxy.example.com" }, false)).toEqual({});
    expect(validateNativeForm({ ...ok, base_url_override: "https://u:p@proxy.example.com" }, false)).toEqual({ base_url_override: MSG.urlUserinfo });
    expect(validateNativeForm({ ...ok, name: " " }, false)).toEqual({ name: MSG.nameRequired });
  });

  it("reconhece o Gemini pela camada compatível com a OpenAI", () => {
    expect(isGeminiCompatUrl("https://generativelanguage.googleapis.com/v1beta/openai/")).toBe(true);
    expect(isGeminiCompatUrl("https://generativelanguage.googleapis.com/v1beta/openai")).toBe(true);
    expect(isGeminiCompatUrl("https://generativelanguage.googleapis.com")).toBe(false);
    expect(isGeminiCompatUrl("https://api.openai.com/v1")).toBe(false);
    expect(isGeminiCompatUrl("não é url")).toBe(false);
  });

  it("os endereços oficiais e as páginas da chave", () => {
    expect(NATIVE_PROVIDERS.anthropic.keyUrl).toBe("https://console.anthropic.com/settings/keys");
    expect(NATIVE_PROVIDERS.google_gemini.keyUrl).toBe("https://aistudio.google.com/apikey");
    expect(NATIVE_PROVIDERS.anthropic.officialUrl).toBe("https://api.anthropic.com");
    expect(NATIVE_PROVIDERS.google_gemini.officialUrl).toBe("https://generativelanguage.googleapis.com");
  });
});

describe("sugestão de modelo por papel (spec 132 T10)", () => {
  const all = (id: string) => ({ architect: id, developer: id, reviewer: id });
  const price = (input: number, output: number) => ({ input_per_mtok: input, output_per_mtok: output });

  it("lista vazia ou sem id válido: sem sugestão", () => {
    expect(suggestRoleModels([])).toBeNull();
    expect(suggestRoleModels([{ id: " " }])).toBeNull();
  });

  it("sem preço para nenhum: o primeiro da ordem do provedor, não o de maior saída", () => {
    expect(suggestRoleModels([{ id: "x" }, { id: "y" }])).toEqual(all("x"));
    expect(suggestRoleModels([{ id: "a", output_tokens: 8 }, { id: "b", output_tokens: 32 }])).toEqual(all("a"));
    expect(suggestRoleModels([{ id: "a" }, { id: "b" }], { outro: price(0.1, 0.1) })).toEqual(all("a"));
  });

  it("com preço: o de menor entrada mais saída; empate fica com a ordem do provedor", () => {
    const models = [{ id: "caro" }, { id: "barato" }, { id: "barato-2" }, { id: "sem-preco" }];
    const prices = { caro: price(15, 75), barato: price(1, 5), "barato-2": price(2, 4) };
    expect(suggestRoleModels(models, prices)).toEqual(all("barato"));
    // Só um candidato tem preço: ele ganha, mesmo depois na ordem.
    expect(suggestRoleModels([{ id: "a" }, { id: "b" }], { b: price(3, 15) })).toEqual(all("b"));
  });

  it("o preço não traz de volta quem não raciocina ou não aceita imagem", () => {
    const models = [
      { id: "sem-raciocinio", thinking: false, vision: true },
      { id: "sem-imagem", thinking: true, vision: false },
      { id: "completo-caro", thinking: true, vision: true },
      { id: "completo", thinking: true, vision: true },
    ];
    const prices = { "sem-raciocinio": price(0.1, 0.1), "sem-imagem": price(0.1, 0.1), "completo-caro": price(10, 10), completo: price(1, 1) };
    expect(suggestRoleModels(models, prices)).toEqual(all("completo"));
    expect(suggestionCandidates(models).map((m) => m.id)).toEqual(["completo-caro", "completo"]);
  });

  it("filtro sem nenhum que passe é ignorado: primeiro cai o de imagem, depois o de raciocínio", () => {
    expect(suggestionCandidates([{ id: "a", thinking: true, vision: false }, { id: "b", thinking: false, vision: false }]).map((m) => m.id)).toEqual(["a"]);
    expect(suggestionCandidates([{ id: "a", thinking: false }, { id: "b", thinking: false }]).map((m) => m.id)).toEqual(["a", "b"]);
  });

  it("o campo ausente em alguns modelos não os elimina quando nenhum o informa", () => {
    expect(suggestionCandidates([{ id: "a" }, { id: "b", thinking: true }]).map((m) => m.id)).toEqual(["b"]);
    expect(suggestionCandidates([{ id: "a" }, { id: "b" }]).map((m) => m.id)).toEqual(["a", "b"]);
  });
});

describe("dica do esforço por provedor (spec 132 T10)", () => {
  it("uma linha por tipo, nos dois idiomas", () => {
    expect(effortHint("anthropic")).toBe("Anthropic: o esforço define quanto o Claude raciocina antes de responder.");
    expect(effortHint("google_gemini")).toContain("Gemini: o esforço vira o nível de raciocínio");
    expect(effortHint("api_key")).toContain("reasoning effort");
    expect(effortHint("chatgpt_plan")).toContain("Plano ChatGPT");
    setLanguage("en", { persist: false });
    expect(effortHint("anthropic")).toBe("Anthropic: the effort sets how much Claude reasons before answering.");
    setLanguage("pt-BR", { persist: false });
  });
});
