/*
 * Textos do Nanty pelo dicionário da web (spec 138 T10, C-08): o porquê, o resultado, as linhas
 * extras da conversa, o plural, os números e o erro da rota, nos dois idiomas.
 */
import { afterEach, describe, expect, it } from "vitest";
import type { Coded } from "../../api/codes";
import { setLanguage } from "../../i18n";
import { nantyCodeText, nantyLinesText, nantyTitle } from "./nantyText";

afterEach(() => setLanguage("pt-BR", { persist: false }));

describe("texto de um código do Nanty", () => {
  it("o porquê com os parâmetros, nos dois idiomas", () => {
    expect(nantyCodeText("nanty.why.plan_waiting", { since: "14:02" })).toBe("Observador de atenção: um plano espera a sua aprovação desde 14:02.");
    expect(nantyCodeText("nanty.why.plan_waiting", { since: "14:02" }, {}, "en")).toBe("Attention observer: a plan has been waiting for your approval since 02:02 PM.");
  });

  it("plural pelo count, com o número no formato do idioma", () => {
    expect(nantyCodeText("nanty.why.cost_alert", { count: 1, since: "09:00" })).toBe("Observador de custo: 1 tarefa passou do patamar de custo, visto às 09:00.");
    expect(nantyCodeText("nanty.why.cost_alert", { count: 1200, since: "09:00" })).toBe("Observador de custo: 1.200 tarefas passaram do patamar de custo, visto às 09:00.");
  });

  it("valor em US$ sempre com duas casas; decimal com a vírgula do pt-BR", () => {
    expect(nantyCodeText("nanty.chat.status_budget_usd", { spent: 0.1, budget: 1 })).toBe("Gasto do modelo hoje: US$ 0,10 de US$ 1,00.");
    expect(nantyCodeText("nanty.chat.status_budget_usd", { spent: 0.1, budget: 1 }, {}, "en")).toBe("Model spend today: US$ 0.10 of US$ 1.00.");
    expect(nantyCodeText("nanty.digest.disk", { free_gb: 12.5 })).toBe("Menor espaço livre em disco: 12,50 GB.");
  });

  it("a lista de comandos segue o idioma do texto, não a gravada pelo servidor", () => {
    const pt = { commands: "status, resumo, pausar, silencio, ajuda" };
    expect(nantyCodeText("nanty.chat.no_model", pt, {}, "en")).toBe("To chat, choose the Nanty model in Settings. The commands work without a model: status, summary, pause, quiet, help.");
    expect(nantyCodeText("nanty.chat.help", { commands: "status, summary, pause, quiet, help" })).toContain("Comandos: status, resumo, pausar, silencio, ajuda.");
  });

  it("a hora do porquê no formato da interface: 24 h em pt-BR, 12 h em en", () => {
    expect(nantyCodeText("nanty.why.disk_low", { min_gb: 20, since: "21:20" })).toBe("Observador de disco: o espaço livre ficou abaixo de 20 GB às 21:20.");
    expect(nantyCodeText("nanty.why.disk_low", { min_gb: 20, since: "21:20" }, {}, "en")).toContain("at 09:20 PM.");
    expect(nantyCodeText("nanty.chat.status_silence", { until: "07:05" }, {}, "en")).toContain("07:05 AM");
    // Fora do formato, como veio.
    expect(nantyCodeText("nanty.chat.status_silence", { until: "amanhã" })).toBe("Avisos em silêncio até amanhã.");
  });

  it("o erro da rota sai pelo serverError, com os parâmetros dele", () => {
    expect(nantyCodeText("nanty.outcome.failed", { error: "nanty_unavailable" })).toBe("Não deu certo: o Nanty está ativo em outra instância do Nants");
    // Código sem tradução: fica o código (nunca um texto em branco).
    expect(nantyCodeText("nanty.outcome.failed", { error: "codigo_que_nao_existe" })).toBe("Não deu certo: codigo_que_nao_existe");
  });

  it("parâmetro faltando ou chave desconhecida: null (a tela mostra a reserva, nunca {x})", () => {
    expect(nantyCodeText("nanty.why.plan_waiting", {})).toBeNull();
    expect(nantyCodeText("nanty.nao.existe", {})).toBeNull();
    expect(nantyCodeText("nanty.why.cost_alert", { since: "1" })).toBeNull();
  });

  it("a mensagem do Nanty: o texto fixo e as linhas extras, cada uma pelo seu código", () => {
    const lines: Coded[] = [
      { code: "nanty.chat.status_silence", params: { until: "22:00" } },
      { code: "nanty.chat.status_no_model", params: {} },
      { code: "nanty.nao.existe", params: {} },
    ];
    expect(nantyLinesText("nanty.chat.status", { waiting: 2, done: 1, planned: 0 }, lines)).toEqual([
      "O Nanty está atento. Esperando você: 2. Feito hoje: 1. Previsto: 0.",
      "Avisos em silêncio até 22:00.",
      "Sem modelo escolhido: os avisos saem com o texto fixo.",
    ]);
    expect(nantyLinesText(null, {}, lines)).toEqual([]);
  });
});

describe("título do cartão", () => {
  it("o tipo do aviso, o nome da ação e a reserva", () => {
    expect(nantyTitle("nanty.why.disk_low")).toBe("Pouco espaço em disco");
    expect(nantyTitle("nanty.card.pause_run")).toBe("Pausar a execução");
    expect(nantyTitle("nanty.action.swap_reserve")).toBe("Trocar para a reserva e seguir");
    expect(nantyTitle("nanty.why.novo_tipo")).toBe("Aviso do Nanty");
    setLanguage("en", { persist: false });
    expect(nantyTitle("nanty.why.disk_low")).toBe("Low disk space");
  });
});
