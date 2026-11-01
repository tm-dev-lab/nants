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
    expect(nantyCodeText("nanty.why.plan_waiting", { since: "14:02" })).toBe("Um plano espera a sua aprovação desde 14:02.");
    expect(nantyCodeText("nanty.why.plan_waiting", { since: "14:02" }, {}, "en")).toBe("A plan has been waiting for your approval since 02:02 PM.");
  });

  it("plural pelo count, com o número no formato do idioma", () => {
    expect(nantyCodeText("nanty.why.cost_alert", { count: 1, since: "09:00" })).toBe("1 tarefa passou do patamar de custo; percebi às 09:00.");
    expect(nantyCodeText("nanty.why.cost_alert", { count: 1200, since: "09:00" })).toBe("1.200 tarefas passaram do patamar de custo; percebi às 09:00.");
  });

  it("valor em US$ sempre com duas casas; decimal com a vírgula do pt-BR", () => {
    expect(nantyCodeText("nanty.chat.status_budget_usd", { spent: 0.1, budget: 1 })).toBe("Hoje gastei US$ 0,10 dos US$ 1,00 do dia com o modelo.");
    expect(nantyCodeText("nanty.chat.status_budget_usd", { spent: 0.1, budget: 1 }, {}, "en")).toBe("I have spent US$ 0.10 of today's US$ 1.00 model budget.");
    expect(nantyCodeText("nanty.digest.disk", { free_gb: 12.5 })).toBe("O disco com menos espaço está com 12,50 GB livres.");
  });

  it("a lista de comandos segue o idioma do texto, não a gravada pelo servidor", () => {
    const pt = { commands: "status, resumo, pausar, silencio, ajuda" };
    expect(nantyCodeText("nanty.chat.no_model", pt, {}, "en")).toBe("To chat freely I need a model: choose one in the Nanty Settings. Meanwhile, these shortcuts work without it: status, summary, pause, quiet, help.");
    expect(nantyCodeText("nanty.chat.help_no_model", pt)).toBe("Para conversar com as suas palavras, eu preciso de um modelo: escolha um nos Ajustes do Nanty. Por enquanto, tenho estes atalhos: status, resumo, pausar, silencio, ajuda.");
    expect(nantyCodeText("nanty.chat.help_no_model", pt, {}, "en")).toBe("To talk to me in your own words, I need a model: choose one in the Nanty Settings. For now, I have these shortcuts: status, summary, pause, quiet, help.");
    expect(nantyCodeText("nanty.chat.help", { commands: "status, summary, pause, quiet, help" })).toContain("tenho estes: status, resumo, pausar, silencio, ajuda.");
  });

  it("a hora do porquê no formato da interface: 24 h em pt-BR, 12 h em en", () => {
    expect(nantyCodeText("nanty.why.disk_low", { min_gb: 20, since: "21:20" })).toBe("O espaço livre em disco ficou abaixo de 20 GB às 21:20.");
    expect(nantyCodeText("nanty.why.disk_low", { min_gb: 20, since: "21:20" }, {}, "en")).toContain("at 09:20 PM.");
    expect(nantyCodeText("nanty.chat.status_silence", { until: "07:05" }, {}, "en")).toContain("07:05 AM");
    // Fora do formato, como veio.
    expect(nantyCodeText("nanty.chat.status_silence", { until: "amanhã" })).toBe("Os avisos estão em silêncio até amanhã.");
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
    // Spec 139 T06: uma frase por fato, sem os zeros.
    expect(nantyLinesText("nanty.chat.status", {}, [{ code: "nanty.chat.status_waiting", params: { count: 2 } }, { code: "nanty.chat.status_done", params: { count: 1 } }, ...lines])).toEqual([
      "Estou acompanhando os projetos por aqui.",
      "2 avisos esperam a sua decisão.",
      "Hoje já cuidei de 1 coisa.",
      "Os avisos estão em silêncio até 22:00.",
      "Ainda não tenho um modelo escolhido, então falo com as frases prontas do Nants.",
    ]);
    expect(nantyLinesText(null, {}, lines)).toEqual([]);
  });

  it("a reserva sem modelo (spec 139 T03): o projeto citado e a fase, nos dois idiomas", () => {
    const lines: Coded[] = [
      { code: "nanty.phase.paused", params: {} },
      { code: "nanty.status.task", params: { task: "T02" } },
      { code: "nanty.status.progress", params: { count: 1, total: 2 } },
      { code: "nanty.status.since", params: { since: "14:02" } },
    ];
    expect(nantyLinesText("nanty.chat.reserve_status", { project: "Test 14" }, lines)).toEqual([
      "Estou sem o modelo agora, mas posso contar o que sei do projeto Test 14:",
      "A execução está pausada numa tarefa que precisa de você.",
      "A tarefa atual é a T02.",
      "1 de 2 tarefas já está pronta.",
      "Está assim desde 14:02.",
    ]);
    expect(nantyLinesText("nanty.chat.reserve_status", { project: "Test 14" }, lines, "en")).toEqual([
      "I cannot reach the model right now, but here is what I know about project Test 14:",
      "The run is paused on a task that needs you.",
      "The current task is T02.",
      "1 of 2 tasks is done.",
      "It has been like this since 02:02 PM.",
    ]);
    // Spec 139 T06, revisão 1: a concordância segue o `count` (as prontas, ou o total quando nenhuma está).
    expect(nantyCodeText("nanty.status.progress", { count: 2, total: 3 })).toBe("2 de 3 tarefas já estão prontas.");
    expect(nantyCodeText("nanty.status.progress_none", { count: 1 })).toBe("A única tarefa ainda não está pronta.");
    expect(nantyCodeText("nanty.status.progress_only", {})).toBe("A única tarefa já está pronta.");
    expect(nantyCodeText("nanty.status.progress_only", {}, {}, "en")).toBe("The only task is done.");
    expect(nantyCodeText("nanty.status.progress_none", { count: 4 }, {}, "en")).toBe("None of the 4 tasks is done yet.");
  });
});

describe("título do cartão", () => {
  it("o tipo do aviso, o nome da ação e a reserva", () => {
    expect(nantyTitle("nanty.why.disk_low")).toBe("Pouco espaço em disco");
    expect(nantyTitle("nanty.card.pause_run")).toBe("Pausar a execução");
    expect(nantyTitle("nanty.action.swap_reserve")).toBe("Trocar para a reserva e seguir");
    expect(nantyTitle("nanty.why.novo_tipo")).toBe("Aviso do Nanty");
    // Spec 139 T04: o silêncio da conversa, com a hora calculada pelo servidor.
    expect(nantyTitle("nanty.card.end_silence")).toBe("Avisos em silêncio");
    expect(nantyTitle("nanty.card.set_silence")).toBe("Silenciar os avisos");
    expect(nantyCodeText("nanty.chat.silence_set", { until: "15:30" })).toBe(
      "Avisos em silêncio até 15:30. O urgente continua chegando, sem som; para voltar antes, use o botão.",
    );
    setLanguage("en", { persist: false });
    expect(nantyTitle("nanty.why.disk_low")).toBe("Low disk space");
    expect(nantyTitle("nanty.card.end_silence")).toBe("Notices muted");
  });
});
