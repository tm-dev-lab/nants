/*
 * Análise de recuperação do Arquiteto no idioma ativo (spec 102, T02 F-01).
 *
 * O servidor (`server/recovery.rs`) grava a análise com cabeçalho e rótulos fixos em pt-BR:
 * "Análise de recuperação #N (DECISÃO)", "- Causa raiz: ...", "- Decisão: ...", o bloco
 * "- Contexto:" da escalada e as linhas de contagem. Esses textos são do Nants: aqui viram o
 * dicionário no idioma ativo. O que o agente escreveu (causa, estratégia, proposta, trechos) fica
 * como veio. Em pt-BR o texto volta idêntico; texto que não é uma análise também.
 */
import { translate, type Lang, type MessageKey } from "./i18n";

const HEADER = /^Análise de recuperação #(\d+) \((\w+)\)/;

/** Linhas `- Rótulo: valor` (com o recuo de origem): o valor é conteúdo e não muda. */
const LABELS: readonly (readonly [string, MessageKey])[] = [
  ["- Causa raiz: ", "recoveryText.rootCause"],
  ["- Estratégia sugerida: ", "recoveryText.suggestedStrategy"],
  ["- Estratégia: ", "recoveryText.strategy"],
  ["- Novo test_command: ", "recoveryText.newTestCommand"],
  ["- Novos checks: ", "recoveryText.newChecks"],
  ["- Novo allowed_scope: ", "recoveryText.newScope"],
  ["- Escopo ampliado: ", "recoveryText.scopeAdded"],
  ["- Escopo não ampliado: ", "recoveryText.scopeNotAdded"],
  ["- Escopo recusado: ", "recoveryText.scopeRefused"],
  ["- Proposta: ", "recoveryText.proposal"],
  ["- Pergunta ao humano: ", "recoveryText.question"],
  ["- Tarefas novas: ", "recoveryText.newTasks"],
  ["- Apontamentos dispensados: ", "recoveryText.waived"],
  ["- Opções descartadas: ", "recoveryText.discarded"],
  ["- comando: ", "recoveryText.command"],
  ["- código: ", "recoveryText.exitCode"],
  ["- por que não resolveu: ", "recoveryText.whyNot"],
];

/** Linhas fixas inteiras (sem o recuo). */
const FIXED: ReadonlyMap<string, MessageKey> = new Map<string, MessageKey>([
  ["- Decisão: configuração de testes revisada (REVISE_CONFIG)", "recoveryText.decision.config"],
  ["- Decisão: tarefa revisada (REVISE_TASK)", "recoveryText.decision.revise"],
  ["- Decisão: nova estratégia (RETRY)", "recoveryText.decision.retry"],
  ["- Decisão: escalar ao humano", "recoveryText.decision.escalate"],
  ["- Decisão: tarefa dividida (SPLIT)", "recoveryText.decision.split"],
  ["- Decisão: critérios ajustados ao que o ambiente prova (ADJUST)", "recoveryText.decision.adjust"],
  ["- Decisão: pergunta ao humano (ASK)", "recoveryText.decision.ask"],
  [
    "- Ajuste não aplicado: os critérios não puderam ser gravados; a tarefa segue com a estratégia (RETRY)",
    "recoveryText.adjustRefused",
  ],
  ["- Configuração de testes corrigida e aplicada pelo servidor", "recoveryText.configApplied"],
  ["Ambiente docker ajustado: o run será relançado com o ambiente novo.", "recoveryText.migrated"],
  ["- Contagem: não conta como ciclo (escalada determinística)", "recoveryText.notCounted"],
  ["- Contexto:", "recoveryText.context"],
  ["- trecho do erro:", "recoveryText.excerpt"],
  ["(sem saída registrada)", "recoveryText.noOutput"],
  ["- Opções para o humano:", "recoveryText.humanOptions"],
  [
    "- opções: escolha uma das opções propostas acima, escreva uma orientação própria ou use Retomar",
    "recoveryText.options.proposed",
  ],
  [
    "- opções: (1) ajustar `test_command`/checks nas configurações para preparar o que falta (ex.: instalar dependências antes dos testes); (2) ajustar o ambiente docker (.nants/) quando a imagem não tem o componente; (3) corrigir o ambiente manualmente e usar Retomar; (4) NÃO remover os testes do comando para contornar o problema",
    "recoveryText.options.environmental",
  ],
  [
    "- opções: (1) orientar o Desenvolvedor com uma estratégia concreta e usar Retomar; (2) revisar o objetivo/critérios da tarefa; (3) decidir o ponto de produto em aberto",
    "recoveryText.options.quality",
  ],
  [
    "- opções: responda à pergunta acima no campo de orientação; a tarefa volta a executar com a sua resposta",
    "recoveryText.options.ask",
  ],
]);

const ROUND = /^- Rodada autônoma \((?:aprovação automática|YOLO)\) #(\d+) de (\d+)$/;
const ENV_FIX = /^- Correção de ambiente \((?:aprovação automática|YOLO)\) #(\d+)$/;
const ATTEMPTED = /^- o que foi tentado: (\d+) de (\d+) ciclos de recuperação e (\d+) de (\d+) rodadas autônomas$/;
const ATTEMPTED_VALUE = /^(\d+) de (\d+) ciclos de recuperação e (\d+) de (\d+) rodadas autônomas$/;

/** Causas fixas do servidor (exatas, ou seguidas de ": detalhe"). */
const CAUSES: ReadonlyMap<string, MessageKey> = new Map<string, MessageKey>([
  ["proposta de configuração de testes inválida", "recoveryText.cause.configInvalid"],
  ["correção da configuração de testes não aprovada", "recoveryText.cause.configDenied"],
  ["falha ao gravar a configuração de testes", "recoveryText.cause.configSave"],
  ["análise indisponível", "recoveryText.cause.unavailable"],
  ["ambiente: o comando de testes não executou", "recoveryText.cause.environment"],
  [
    "ambiente: reescrever a tarefa não resolve falha de ambiente (a verificação não pode ser rebaixada)",
    "recoveryText.cause.envRevise",
  ],
  ["a proposta removeria verificação", "recoveryText.cause.configWeakens"],
  ["a proposta repetiria o comando de testes", "recoveryText.cause.configRepeats"],
  ["ambiente: limite de rodadas autônomas atingido sem resolver a falha", "recoveryText.cause.envRounds"],
  ["limite de ciclos de recuperação atingido", "recoveryText.cause.limit"],
  ["ajuste do ambiente docker não aplicado", "recoveryText.cause.migrationFailed"],
  ["aprovação do ambiente negada pelo humano", "recoveryText.cause.envDenied"],
  ["docker indisponível", "recoveryText.cause.dockerUnavailable"],
]);
const CITED = "(citada pelo modelo) ";

/**
 * Causa raiz no idioma ativo (spec 115 T04): as causas fixas do servidor (`CAUSE_*` de
 * `server/recovery.rs`, gravadas em pt-BR porque o servidor as compara) viram o dicionário, na
 * análise da Conversa e no cartão de recuperação; o que o modelo escreveu fica como veio.
 */
export function localizeRecoveryCause(value: string, lang: Lang): string {
  let rest = value;
  let prefix = "";
  if (rest.startsWith(CITED)) {
    prefix = `${translate(lang, "recoveryText.citedByModel")} `;
    rest = rest.slice(CITED.length);
  }
  for (const [pt, key] of CAUSES) {
    if (rest === pt) return prefix + translate(lang, key);
    if (rest.startsWith(`${pt}: `)) return `${prefix}${translate(lang, key)}${rest.slice(pt.length)}`;
  }
  return prefix + rest;
}

function line(raw: string, lang: Lang): string {
  const indent = /^\s*/.exec(raw)?.[0] ?? "";
  const body = raw.slice(indent.length);
  const fixed = FIXED.get(body);
  if (fixed !== undefined) return indent + translate(lang, fixed);
  let m = ROUND.exec(body);
  if (m !== null) return indent + translate(lang, "recoveryText.autonomousRound", { n: m[1] ?? "", max: m[2] ?? "" });
  m = ENV_FIX.exec(body);
  if (m !== null) return indent + translate(lang, "recoveryText.envFix", { n: m[1] ?? "" });
  m = ATTEMPTED.exec(body);
  if (m !== null) {
    return indent + translate(lang, "recoveryText.attempted", { cycles: m[1] ?? "", maxCycles: m[2] ?? "", rounds: m[3] ?? "", maxRounds: m[4] ?? "" });
  }
  for (const [pt, key] of LABELS) {
    if (!body.startsWith(pt)) continue;
    const value = body.slice(pt.length);
    const shown = key === "recoveryText.rootCause" || key === "recoveryText.whyNot" ? localizeRecoveryCause(value, lang) : value;
    return `${indent}- ${translate(lang, key)}: ${shown}`;
  }
  return raw;
}

/** Valor de "o que foi tentado" (a contagem do servidor) no idioma ativo; outro texto volta igual. */
export function localizeAttempted(value: string, lang: Lang): string {
  const m = ATTEMPTED_VALUE.exec(value.trim());
  if (m === null) return value;
  return translate(lang, "recoveryText.attemptedValue", { cycles: m[1] ?? "", maxCycles: m[2] ?? "", rounds: m[3] ?? "", maxRounds: m[4] ?? "" });
}

/** Análise de recuperação com os textos fixos em `lang`; outro texto (ou pt-BR) volta igual. */
export function localizeRecoveryAnalysis(text: string, lang: Lang): string {
  if (lang === "pt-BR") return text;
  const header = HEADER.exec(text);
  if (header === null) return text;
  const head = translate(lang, "recoveryText.header", { cycle: header[1] ?? "", decision: header[2] ?? "" });
  const rest = text.slice(header[0].length);
  return head + rest.split("\n").map((l, i) => (i === 0 ? l : line(l, lang))).join("\n");
}
