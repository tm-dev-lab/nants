/*
 * Linhas do "Mudou nesta versão" (`plan_changes`, spec 131 T06): o servidor manda `code` e
 * `params` neutros (`sdd::plan_diff::ChangeLine`), e a tela monta o texto pelo dicionário
 * (`planChange.<code>`), no idioma ativo. Os arquivos gravados antes trazem a linha em texto, que
 * aparece como veio.
 */
import { parseCoded } from "../api/codes";
import type { PlanChangeEntry } from "../api/types";
import { DICTIONARIES, translate, translatePlural, type Lang, type MessageKey } from "./index";

/** Códigos com a contagem de uma lista (`added`, `removed`; os dois em zero = reordenados). */
const DELTA_CODES = new Set(["global_criteria", "task_criteria"]);

function count(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? Math.floor(value) : 0;
}

/** Texto da linha no idioma `lang`; linha com código sem chave no dicionário, `null`. */
export function planChangeText(entry: PlanChangeEntry, lang: Lang): string | null {
  if (typeof entry === "string") return entry;
  const coded = parseCoded(entry.code, entry.params);
  if (coded === undefined) return null;
  const key = `planChange.${coded.code}`;
  if (!Object.hasOwn(DICTIONARIES[lang], key)) return null;
  const params: Record<string, string | number> = { ...coded.params };
  if (DELTA_CODES.has(coded.code)) {
    const [added, removed] = [count(coded.params.added), count(coded.params.removed)];
    const parts = [
      ...(added > 0 ? [translatePlural(lang, "planChange.added", added)] : []),
      ...(removed > 0 ? [translatePlural(lang, "planChange.removed", removed)] : []),
    ];
    params.delta = parts.length > 0 ? parts.join(", ") : translate(lang, "planChange.reordered");
  }
  return translate(lang, key as MessageKey, params);
}
