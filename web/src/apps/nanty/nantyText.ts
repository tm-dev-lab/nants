/*
 * Textos do Nanty na janela (spec 138 T10, C-08): o servidor grava `code` + `params` e a web monta
 * o texto pelo dicionário dela, no idioma ativo. O dicionário da web tem as mesmas chaves `nanty.*`
 * do servidor (o porquê, as ações, os resultados, a conversa, os comandos, o resumo e a voz), com o
 * mesmo texto (trava `tests/i18n_parity.rs`).
 *
 * - `{count}` com as formas `.one`/`.other`: o plural do idioma;
 * - números: inteiros com o separador do idioma; decimais e valores em "US$ {x}" com duas casas;
 * - `{error}`: o código de erro da API (`serverError.*`), traduzido com os `error_params`;
 * - `{commands}`: a lista de comandos do idioma do texto (o servidor grava a do idioma do Nanty);
 * - `{since}` e `{until}` (`HH:MM` local): no formato de hora da interface, como a hora do cartão;
 * - parâmetro do texto que falta: `null` (a tela mostra o texto de reserva, nunca `{x}`).
 */
import type { CodeParams, Coded } from "../../api/codes";
import { DICTIONARIES, formatNumberIn, formatTimeIn, getLanguage, pluralForm, type Lang, type MessageKey } from "../../i18n";
import { translateCode } from "../../i18n/serverCodes";

const PARAM_RE = /\{(\w+)\}/g;

function has(lang: Lang, key: string): key is MessageKey {
  return Object.hasOwn(DICTIONARIES[lang], key);
}

/** Chave do código: a própria, ou a forma do plural quando só as duas formas existem. */
function keyOf(lang: Lang, code: string, params: CodeParams): MessageKey | null {
  if (has(lang, code)) return code;
  const n = params.count;
  if (typeof n !== "number") return null;
  const plural = `${code}.${pluralForm(lang, n)}`;
  return has(lang, plural) ? plural : null;
}

function numberText(lang: Lang, value: number, money: boolean): string {
  if (money || !Number.isInteger(value)) return formatNumberIn(lang, value, { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  return formatNumberIn(lang, value);
}

/** Os comandos fixos (T08), na ordem da ajuda, com os nomes do idioma (sem a barra). */
const COMMAND_KEYS: readonly MessageKey[] = ["nanty.convo.cmd.status", "nanty.convo.cmd.summary", "nanty.convo.cmd.pause", "nanty.convo.cmd.silence", "nanty.convo.cmd.help"];

function commandNames(lang: Lang): string {
  return COMMAND_KEYS.map((key) => DICTIONARIES[lang][key].replace(/^\//, "")).join(", ");
}

/** `HH:MM` no formato de hora do idioma (12 h em en); fora do formato, como veio. */
export function clockText(value: string, lang: Lang = getLanguage()): string {
  const m = /^(\d{2}):(\d{2})$/.exec(value);
  if (m === null) return value;
  const at = new Date(2000, 0, 1, Number(m[1]), Number(m[2]));
  return formatTimeIn(lang, at);
}

/** Texto de um código `nanty.*` com os parâmetros, em `lang`; `null` sem chave ou sem parâmetro. */
export function nantyCodeText(code: string, params: CodeParams = {}, errorParams: CodeParams = {}, lang: Lang = getLanguage()): string | null {
  const key = keyOf(lang, code, params);
  if (key === null) return null;
  const template = DICTIONARIES[lang][key];
  let missing = false;
  const text = template.replace(PARAM_RE, (whole, name: string) => {
    const value = Object.hasOwn(params, name) ? params[name] : undefined;
    if (value === undefined) {
      missing = true;
      return whole;
    }
    if (name === "error" && typeof value === "string") return translateCode("serverError", value, errorParams, lang) ?? value;
    // A lista de comandos segue o idioma do texto (o servidor grava a do idioma do Nanty).
    if (name === "commands") return commandNames(lang);
    // Hora local `HH:MM` gravada pelo servidor: no formato de hora da interface.
    if ((name === "since" || name === "until") && typeof value === "string") return clockText(value, lang);
    if (typeof value === "number") return numberText(lang, value, template.includes(`US$ {${name}}`));
    return value;
  });
  return missing ? null : text;
}

/** O texto de uma mensagem do Nanty: o texto fixo e as linhas extras, cada um no seu parágrafo. */
export function nantyLinesText(code: string | null, params: CodeParams, lines: readonly Coded[], lang: Lang = getLanguage()): string[] {
  if (code === null) return [];
  const out: string[] = [];
  const main = nantyCodeText(code, params, {}, lang);
  if (main !== null) out.push(main);
  for (const line of lines) {
    const text = nantyCodeText(line.code, line.params, {}, lang);
    if (text !== null) out.push(text);
  }
  return out;
}

/** Sufixo do código depois do prefixo da família (`nanty.why.disk_low` -> `disk_low`). */
function suffix(code: string, prefix: string): string | null {
  return code.startsWith(prefix) ? code.slice(prefix.length) : null;
}

/**
 * Título curto de um cartão ou de um "feito", pelo código: o nome da ação (`nanty.action.*`) ou o
 * título do tipo de aviso (`nanty.title.*`, para `nanty.why.*` e `nanty.card.*`).
 */
export function nantyTitle(code: string, lang: Lang = getLanguage()): string {
  if (code.startsWith("nanty.action.") && has(lang, code)) return DICTIONARIES[lang][code];
  const kind = suffix(code, "nanty.why.") ?? suffix(code, "nanty.card.");
  const key = kind === null ? null : `nanty.title.${kind}`;
  return key !== null && has(lang, key) ? DICTIONARIES[lang][key] : DICTIONARIES[lang]["nanty.title.unknown"];
}
