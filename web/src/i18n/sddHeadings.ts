/*
 * Títulos canônicos do `spec.md` e das tarefas no idioma ativo (spec 131 T06, C-12).
 *
 * O servidor e o Arquiteto gravam os títulos das seções em pt-BR ("Critérios de Aceitação",
 * "Registro de Apontamentos"...) em qualquer projeto: muitos leitores do servidor procuram a seção
 * por esse título, e a gravação no idioma do projeto ficou para uma spec futura. Aqui só a
 * EXIBIÇÃO muda: o título reconhecido em qualquer idioma do dicionário (`sddHeading.*`) sai no
 * idioma ativo; o arquivo não é tocado. Título fora da tabela aparece como está.
 */
// Os dicionários vêm direto dos módulos (e não de `./index`): `./serverCodes` usa este módulo, e
// `./index` chega a ele pelo cliente da API, num ciclo de importação.
import { en } from "./en";
import { LANGS, type Lang } from "./lang";
import { ptBR, type MessageKey } from "./ptBR";

const DICTIONARIES: Readonly<Record<Lang, Readonly<Record<MessageKey, string>>>> = { "pt-BR": ptBR, en };

/** Títulos fixos, em qualquer nível de cabeçalho. */
const FIXED_KEYS: readonly MessageKey[] = [
  "sddHeading.context",
  "sddHeading.approach",
  "sddHeading.premises",
  "sddHeading.globalCriteria",
  "sddHeading.taskPlan",
  "sddHeading.roleGuidance",
  "sddHeading.roleDeveloper",
  "sddHeading.roleReviewer",
  "sddHeading.installFootprint",
  "sddHeading.install",
  "sddHeading.flaky",
  "sddHeading.reuse",
  "sddHeading.format",
  "sddHeading.objective",
  "sddHeading.criteria",
  "sddHeading.history",
  "sddHeading.findings",
  "sddHeading.findingsLog",
  "sddHeading.registry",
  "sddHeading.responses",
  "sddHeading.advice",
  "sddHeading.humanGuidance",
];

/** Títulos com um valor no meio ("Tentativa 2", "Pergunta (developer)"). */
const PATTERN_KEYS: readonly MessageKey[] = [
  "sddHeading.numbered.attempt",
  "sddHeading.numbered.rejection",
  "sddHeading.numbered.recovery",
  "sddHeading.numbered.question",
];

/** Caixa e espaços não contam ("Critérios de aceitação" casa com "Critérios de Aceitação"). */
function fold(text: string): string {
  return text.trim().replace(/\s+/g, " ").toLocaleLowerCase();
}

/** Título fixo (forma dobrada, em qualquer idioma) → chave. */
const FIXED: ReadonlyMap<string, MessageKey> = (() => {
  const out = new Map<string, MessageKey>();
  for (const key of FIXED_KEYS) {
    for (const lang of LANGS) out.set(fold(DICTIONARIES[lang][key]), key);
  }
  return out;
})();

interface Pattern {
  key: MessageKey;
  name: string;
  re: RegExp;
}

function escapeRe(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Cada modelo com parâmetro, em cada idioma, vira uma expressão que captura o valor. */
const PATTERNS: readonly Pattern[] = PATTERN_KEYS.flatMap((key) =>
  LANGS.flatMap((lang) => {
    const template = DICTIONARIES[lang][key];
    const match = /\{(\w+)\}/.exec(template);
    if (match === null || match[1] === undefined) return [];
    const [before, after] = [template.slice(0, match.index), template.slice(match.index + match[0].length)];
    const value = match[1] === "n" ? "(\\d+)" : "(.+?)";
    return [{ key, name: match[1], re: new RegExp(`^${escapeRe(before)}${value}${escapeRe(after)}$`, "i") }];
  }),
);

/** Título de seção no idioma `lang`; fora da tabela, o próprio título. */
export function localizeHeadingTitle(title: string, lang: Lang): string {
  const fixed = FIXED.get(fold(title));
  if (fixed !== undefined) return DICTIONARIES[lang][fixed];
  const trimmed = title.trim();
  for (const pattern of PATTERNS) {
    const found = pattern.re.exec(trimmed);
    if (found?.[1] !== undefined) return DICTIONARIES[lang][pattern.key].replace(`{${pattern.name}}`, found[1]);
  }
  return title;
}

const HEADING = /^(\s{0,3}#{1,6}[ \t]+)(.*?)([ \t]*)$/;
const FENCE = /^\s{0,3}(`{3,}|~{3,})/;

/**
 * Markdown com os títulos canônicos no idioma `lang`, linha a linha (a contagem de linhas não muda)
 * e fora dos blocos de código.
 */
export function localizeSddHeadings(markdown: string, lang: Lang): string {
  let fence: string | null = null;
  return markdown
    .split("\n")
    .map((line) => {
      const opener = FENCE.exec(line)?.[1];
      if (opener !== undefined) {
        if (fence === null) fence = opener[0] ?? null;
        else if (opener[0] === fence) fence = null;
        return line;
      }
      if (fence !== null) return line;
      const heading = HEADING.exec(line);
      if (heading === null) return line;
      const [, marks = "", title = "", tail = ""] = heading;
      const local = localizeHeadingTitle(title, lang);
      return local === title ? line : `${marks}${local}${tail}`;
    })
    .join("\n");
}

/** `spec.md` ou tarefa de uma spec (`.sdd/specs/<id>/spec.md`, `.sdd/specs/<id>/tasks/<x>.md`). */
export function isSddDocumentPath(path: string): boolean {
  return /(^|\/)\.sdd\/specs\/[^/]+\/(spec\.md|tasks\/[^/]+\.md)$/.test(path.replace(/\\/g, "/"));
}
