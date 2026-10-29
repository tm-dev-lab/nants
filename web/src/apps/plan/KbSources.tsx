/*
 * Quadro "Fontes da base" do Plano (spec 136 T04, critério 2), visível sem expandir: as citações
 * `kb:` da spec e das tarefas, agrupadas por documento, com a seção, os lugares de uso e se a
 * citação é regra (transcrita num critério, nas Premissas ou nas Orientações, C-04) ou consulta.
 * Um toque abre o Conhecimento na seção, que rola até ela e a destaca por um instante. A fonte que
 * saiu da base fica marcada e não abre. Com a base vazia, nada; com a base e sem citação, uma
 * linha discreta.
 */
import { useId } from "react";
import type { KbPlanSource } from "../../api/kb";
import { Icon } from "../../desktop/Icon";
import { useShell } from "../../desktop/shellContext";
import { useT } from "../../i18n/react";
import { useKb } from "../knowledge/kbStore";
import { kindIcon } from "../knowledge/knowledgeModel";
import { requestOpenKbDoc } from "../knowledge/openRequest";
import { groupKbSources, useLabels } from "./kbSourcesModel";

/** Id do documento sem o sufixo hexadecimal: o nome que sobra de um documento removido. */
function docSlug(id: string): string {
  const cut = id.lastIndexOf("-");
  return cut > 0 ? id.slice(0, cut) : id;
}

export function KindPill({ binding }: { binding: boolean }) {
  const { t } = useT();
  return (
    <span
      className="nd-pill nd-pill-sm nd-pill-nowrap nd-kb-src-kind"
      data-tone={binding ? "violet" : undefined}
      data-kb-src-kind={binding ? "binding" : "reference"}
      title={binding ? t("plan.sources.bindingHint") : t("plan.sources.referenceHint")}
    >
      {binding ? <Icon name="check" size={14} /> : null}
      {binding ? t("plan.sources.binding") : t("plan.sources.reference")}
    </span>
  );
}

function SourceRow({ projectId, source, docTitle }: { projectId: string; source: KbPlanSource; docTitle: string }) {
  const { t, lang } = useT();
  const shell = useShell();
  const section = source.anchor === null ? t("plan.sources.wholeDoc") : (source.section_title ?? source.anchor);
  const where = useLabels(source.uses, lang).join(", ");
  const content = (
    <>
      <span className="nd-kb-src-section">{section}</span>
      <KindPill binding={source.binding} />
      <span className="nd-kb-src-where">{where}</span>
    </>
  );
  if (source.removed) {
    return (
      <li className="nd-kb-src-item" data-removed="true" title={t("plan.sources.removedHint")}>
        {content}
      </li>
    );
  }
  const anchor = source.resolved_anchor ?? source.anchor;
  return (
    <li>
      <button
        type="button"
        className="nd-kb-src-item"
        data-kb-src={source.citation}
        aria-label={anchor === null ? t("plan.kb.open", { doc: docTitle }) : t("plan.kb.openSection", { doc: docTitle, section })}
        onClick={() => {
          requestOpenKbDoc(projectId, source.doc, anchor);
          shell.openWindow("knowledge");
        }}
      >
        {content}
        <Icon name="chevron-right" size={14} className="nd-kb-src-go" />
      </button>
    </li>
  );
}

export function KbSources({ projectId, sources }: { projectId: string; sources: readonly KbPlanSource[] | null }) {
  const { t, tp } = useT();
  const headingId = useId();
  const kb = useKb(projectId);
  if (sources === null || kb?.status !== "ok") return null;
  const docs = kb.list.docs;
  if (sources.length === 0) {
    if (docs.length === 0) return null;
    return (
      <p className="nd-kb-sources-none" data-kb-sources-none="">
        <Icon name="knowledge" size={14} />
        <span>{tp("plan.sources.unused", docs.length)}</span>
      </p>
    );
  }
  const groups = groupKbSources(sources);
  return (
    <section aria-labelledby={headingId} className="nd-kb-sources" data-kb-sources="">
      <header className="nd-kb-sources-head">
        <Icon name="knowledge" size={16} className="nd-kb-sources-icon" />
        <h3 id={headingId} className="nd-plan-heading">
          {t("plan.sources.title")}
        </h3>
        <span className="nd-kb-sources-count">
          {tp("plan.sources.docs", groups.length)} · {tp("plan.sources.cites", sources.length)}
        </span>
      </header>
      <ul className="nd-kb-sources-docs">
        {groups.map((group) => {
          const kbDoc = docs.find((d) => d.id === group.doc) ?? null;
          const title = group.title ?? t("plan.sources.removedDoc");
          const gone = kbDoc === null;
          return (
            <li key={group.doc} className="nd-kb-src-doc" data-kb-src-doc={group.doc} data-removed={gone ? "true" : undefined}>
              <div className="nd-kb-src-doc-head">
                <Icon name={kbDoc === null ? "file" : kindIcon(kbDoc)} size={14} className="nd-kb-src-doc-icon" />
                <span className="nd-kb-src-doc-title">{title}</span>
                {group.title === null ? <span className="nd-kb-src-doc-id">{docSlug(group.doc)}</span> : null}
                {gone || group.sources.some((s) => s.removed) ? (
                  <span className="nd-pill nd-pill-sm nd-pill-nowrap" data-tone="rose" title={t("plan.sources.removedHint")}>
                    {t("plan.sources.removed")}
                  </span>
                ) : null}
              </div>
              <ul className="nd-kb-src-items">
                {group.sources.map((source) => (
                  <SourceRow key={source.citation} projectId={projectId} source={source} docTitle={title} />
                ))}
              </ul>
            </li>
          );
        })}
      </ul>
      <p className="nd-kb-sources-legend">
        <span className="nd-kb-sources-legend-item">
          <KindPill binding />
          <span>{t("plan.sources.legend.binding")}</span>
        </span>
        <span className="nd-kb-sources-legend-item">
          <KindPill binding={false} />
          <span>{t("plan.sources.legend.reference")}</span>
        </span>
      </p>
    </section>
  );
}
