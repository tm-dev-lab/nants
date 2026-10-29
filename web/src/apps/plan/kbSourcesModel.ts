/*
 * Modelo puro do quadro "Fontes da base" do Plano (spec 136 T04): as citações agrupadas por
 * documento, na ordem em que aparecem no plano, e os lugares de uso no idioma ativo.
 */
import type { KbCitationUse, KbPlanSource } from "../../api/kb";
import { t } from "../../i18n";
import type { Lang } from "../../i18n/lang";
import { localizeHeadingTitle } from "../../i18n/sddHeadings";

/** As citações de um documento. */
interface KbSourceGroup {
  doc: string;
  /** Título do documento; `null` quando ele saiu da base. */
  title: string | null;
  sources: KbPlanSource[];
}

/** Agrupa por documento, na ordem da primeira citação de cada um. */
export function groupKbSources(sources: readonly KbPlanSource[]): KbSourceGroup[] {
  const groups: KbSourceGroup[] = [];
  for (const source of sources) {
    const group = groups.find((g) => g.doc === source.doc);
    if (group === undefined) groups.push({ doc: source.doc, title: source.doc_title, sources: [source] });
    else {
      group.sources.push(source);
      if (group.title === null) group.title = source.doc_title;
    }
  }
  return groups;
}

/** Um lugar de uso: "Premissas" ou "T02 · Critérios de Aceitação", sem repetir. */
export function useLabels(uses: readonly KbCitationUse[], lang: Lang): string[] {
  const out: string[] = [];
  for (const use of uses) {
    const raw = use.section ?? t("plan.sources.intro");
    const section = use.section !== null && use.structural ? localizeHeadingTitle(use.section, lang) : raw;
    const label = use.in === "task" && use.task !== undefined ? t("plan.sources.taskUse", { task: use.task, section }) : section;
    if (!out.includes(label)) out.push(label);
  }
  return out;
}

/** Citações de cada tarefa (para a marca na lista de tarefas). */
export function sourcesByTask(sources: readonly KbPlanSource[]): Map<string, number> {
  const out = new Map<string, number>();
  for (const source of sources) {
    const tasks = new Set(source.uses.flatMap((u) => (u.in === "task" && u.task !== undefined ? [u.task] : [])));
    for (const task of tasks) out.set(task, (out.get(task) ?? 0) + 1);
  }
  return out;
}
