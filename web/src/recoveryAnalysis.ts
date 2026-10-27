/*
 * Análise de recuperação do Arquiteto no idioma ativo (spec 102, T02 F-01).
 *
 * O servidor (`server/recovery.rs`) grava a análise com cabeçalho e rótulos fixos:
 * "Análise de recuperação #N (DECISÃO)", "- Causa raiz: ...", "- Decisão: ...", o bloco
 * "- Contexto:" da escalada e as linhas de contagem. Esses textos são do Nants: aqui viram o
 * dicionário no idioma ativo. O que o agente escreveu (causa, estratégia, proposta, trechos) fica
 * como veio. Texto que não é uma análise volta idêntico.
 *
 * Spec 129 T04: os rótulos são reconhecidos pelas formas do dicionário em TODOS os idiomas
 * (`recoveryText.*`, iterando `DICTIONARIES`), nunca por uma lista avulsa: a análise gravada em pt
 * ou em inglês aparece no idioma ativo, e no mesmo idioma volta idêntica.
 */
import { DICTIONARIES, translate, type Lang, type MessageKey } from "./i18n";

/** Formas da chave em todos os idiomas do dicionário, sem repetição. */
function forms(key: MessageKey): string[] {
  const out: string[] = [];
  for (const dictionary of Object.values(DICTIONARIES)) {
    const form = dictionary[key];
    if (!out.includes(form)) out.push(form);
  }
  return out;
}

function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Molde do dicionário como expressão: cada `{param}` vira o grupo `group(param)`, na ordem. */
function templateSource(template: string, group: (param: string) => string): { source: string; params: string[] } {
  const params: string[] = [];
  const source = escapeRegExp(template).replace(/\\\{([A-Za-z]+)\\\}/g, (_, name: string) => {
    params.push(name);
    return group(name);
  });
  return { source, params };
}

/** Uma forma de um molde: a expressão (linha inteira) e a ordem dos parâmetros. */
interface TemplateForm {
  re: RegExp;
  params: string[];
}

/** Formas de um molde com parâmetros numéricos (`#{n} de {max}`), em todos os idiomas. */
function numericForms(key: MessageKey): TemplateForm[] {
  return forms(key).map((template) => {
    const { source, params } = templateSource(template, () => "(\\d+)");
    return { re: new RegExp(`^${source}$`), params };
  });
}

/** Valores dos parâmetros pelo nome, a partir da primeira forma que casa com `text`. */
function matchTemplate(text: string, candidates: readonly TemplateForm[]): Record<string, string> | null {
  for (const { re, params } of candidates) {
    const m = re.exec(text);
    if (m === null) continue;
    const values: Record<string, string> = {};
    params.forEach((name, i) => {
      values[name] = m[i + 1] ?? "";
    });
    return values;
  }
  return null;
}

/** Cabeçalho `Análise de recuperação #N (DECISÃO)` em qualquer idioma, no início do texto. */
const HEADERS: readonly TemplateForm[] = forms("recoveryText.header").map((template) => {
  const { source, params } = templateSource(template, (name) => (name === "cycle" ? "(\\d+)" : "(\\w+)"));
  return { re: new RegExp(`^${source}`), params };
});

/** Rótulos das linhas `- Rótulo: valor` (com o recuo de origem): o valor é conteúdo e não muda. */
const LABEL_KEYS: readonly MessageKey[] = [
  "recoveryText.rootCause",
  "recoveryText.suggestedStrategy",
  "recoveryText.strategy",
  "recoveryText.newTestCommand",
  "recoveryText.newChecks",
  "recoveryText.newScope",
  "recoveryText.scopeAdded",
  "recoveryText.scopeNotAdded",
  "recoveryText.scopeRefused",
  "recoveryText.proposal",
  "recoveryText.question",
  "recoveryText.newTasks",
  "recoveryText.waived",
  "recoveryText.discarded",
  "recoveryText.command",
  "recoveryText.exitCode",
  "recoveryText.whyNot",
];
/** `- <rótulo>: ` de cada idioma, com a chave. */
const LABELS: readonly (readonly [string, MessageKey])[] = LABEL_KEYS.flatMap((key) =>
  forms(key).map((label): readonly [string, MessageKey] => [`- ${label}: `, key]),
);

/** Chaves das linhas fixas inteiras (sem o recuo). */
const FIXED_KEYS: readonly MessageKey[] = [
  "recoveryText.decision.config",
  "recoveryText.decision.revise",
  "recoveryText.decision.retry",
  "recoveryText.decision.escalate",
  "recoveryText.decision.split",
  "recoveryText.decision.adjust",
  "recoveryText.decision.ask",
  "recoveryText.adjustRefused",
  "recoveryText.configApplied",
  "recoveryText.migrated",
  "recoveryText.notCounted",
  "recoveryText.context",
  "recoveryText.excerpt",
  "recoveryText.noOutput",
  "recoveryText.humanOptions",
  "recoveryText.options.proposed",
  "recoveryText.options.environmental",
  "recoveryText.options.quality",
  "recoveryText.options.ask",
];
const FIXED: ReadonlyMap<string, MessageKey> = new Map(FIXED_KEYS.flatMap((key) => forms(key).map((text): [string, MessageKey] => [text, key])));

const ROUND = numericForms("recoveryText.autonomousRound");
/** Forma antiga da rodada autônoma (antes da aprovação automática), ainda lida nos registros já gravados. */
const LEGACY_ROUND: TemplateForm = { re: /^- Rodada autônoma \(YOLO\) #(\d+) de (\d+)$/, params: ["n", "max"] };
const ENV_FIX = numericForms("recoveryText.envFix");
const LEGACY_ENV_FIX: TemplateForm = { re: /^- Correção de ambiente \(YOLO\) #(\d+)$/, params: ["n"] };
const ATTEMPTED = numericForms("recoveryText.attempted");
const ATTEMPTED_VALUE = numericForms("recoveryText.attemptedValue");

/** Causas fixas do servidor (exatas, ou seguidas de ": detalhe"), em todos os idiomas. */
const CAUSE_KEYS: readonly MessageKey[] = [
  "recoveryText.cause.configInvalid",
  "recoveryText.cause.configDenied",
  "recoveryText.cause.configSave",
  "recoveryText.cause.unavailable",
  "recoveryText.cause.environment",
  "recoveryText.cause.envRevise",
  "recoveryText.cause.configWeakens",
  "recoveryText.cause.configRepeats",
  "recoveryText.cause.envRounds",
  "recoveryText.cause.limit",
  "recoveryText.cause.migrationFailed",
  "recoveryText.cause.envDenied",
  "recoveryText.cause.dockerUnavailable",
];
const CAUSES: readonly (readonly [string, MessageKey])[] = CAUSE_KEYS.flatMap((key) =>
  forms(key).map((text): readonly [string, MessageKey] => [text, key]),
);
const CITED = forms("recoveryText.citedByModel").map((text) => `${text} `);

/** Começo fixo do cabeçalho (`Análise de recuperação #`), em cada idioma. */
const HEADER_STARTS = forms("recoveryText.header").map((template) => template.slice(0, template.indexOf("{")));

/** O texto é uma análise de recuperação (começa pelo cabeçalho, em qualquer idioma). */
export function isRecoveryAnalysis(text: string): boolean {
  return HEADER_STARTS.some((start) => start !== "" && text.startsWith(start));
}

/**
 * Causa raiz no idioma ativo (spec 115 T04): as causas fixas do servidor (`CAUSE_*` de
 * `server/recovery.rs`, gravadas pelo servidor porque ele as compara) viram o dicionário, na
 * análise da Conversa e no cartão de recuperação; o que o modelo escreveu fica como veio.
 */
export function localizeRecoveryCause(value: string, lang: Lang): string {
  let rest = value;
  let prefix = "";
  const cited = CITED.find((mark) => rest.startsWith(mark));
  if (cited !== undefined) {
    prefix = `${translate(lang, "recoveryText.citedByModel")} `;
    rest = rest.slice(cited.length);
  }
  for (const [form, key] of CAUSES) {
    if (rest === form) return prefix + translate(lang, key);
    if (rest.startsWith(`${form}: `)) return `${prefix}${translate(lang, key)}${rest.slice(form.length)}`;
  }
  return prefix + rest;
}

function line(raw: string, lang: Lang): string {
  const indent = /^\s*/.exec(raw)?.[0] ?? "";
  const body = raw.slice(indent.length);
  const fixed = FIXED.get(body);
  if (fixed !== undefined) return indent + translate(lang, fixed);
  const round = matchTemplate(body, [...ROUND, LEGACY_ROUND]);
  if (round !== null) return indent + translate(lang, "recoveryText.autonomousRound", { n: round.n ?? "", max: round.max ?? "" });
  const envFix = matchTemplate(body, [...ENV_FIX, LEGACY_ENV_FIX]);
  if (envFix !== null) return indent + translate(lang, "recoveryText.envFix", { n: envFix.n ?? "" });
  const attempted = matchTemplate(body, ATTEMPTED);
  if (attempted !== null) return indent + translate(lang, "recoveryText.attempted", attempted);
  for (const [label, key] of LABELS) {
    if (!body.startsWith(label)) continue;
    const value = body.slice(label.length);
    const shown = key === "recoveryText.rootCause" || key === "recoveryText.whyNot" ? localizeRecoveryCause(value, lang) : value;
    return `${indent}- ${translate(lang, key)}: ${shown}`;
  }
  return raw;
}

/** Valor de "o que foi tentado" (a contagem do servidor) no idioma ativo; outro texto volta igual. */
export function localizeAttempted(value: string, lang: Lang): string {
  const values = matchTemplate(value.trim(), ATTEMPTED_VALUE);
  if (values === null) return value;
  return translate(lang, "recoveryText.attemptedValue", values);
}

/** Análise de recuperação com os textos fixos em `lang`; outro texto volta igual. */
export function localizeRecoveryAnalysis(text: string, lang: Lang): string {
  for (const { re, params } of HEADERS) {
    const header = re.exec(text);
    if (header === null) continue;
    const values: Record<string, string> = {};
    params.forEach((name, i) => {
      values[name] = header[i + 1] ?? "";
    });
    const head = translate(lang, "recoveryText.header", { cycle: values.cycle ?? "", decision: values.decision ?? "" });
    const rest = text.slice(header[0].length);
    return head + rest.split("\n").map((l, i) => (i === 0 ? l : line(l, lang))).join("\n");
  }
  return text;
}
