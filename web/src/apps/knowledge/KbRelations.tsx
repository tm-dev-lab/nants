/*
 * Ligações do documento no fim da prévia (spec 136 T07, G5 e G8), como os "backlinks" do
 * Obsidian, a partir do `GET /kb/docs/:doc` (T03):
 * - **Usado nos planos:** cada spec que cita o documento, com a seção citada, onde no plano e o
 *   selo Regra ou Consulta (C-04); a seção leva ao trecho na prévia;
 * - **Citado por:** os documentos da base que apontam para este, com a linha em volta do link;
 * - **Links para:** os documentos da base para onde este aponta e os alvos de fora (página ou
 *   arquivo que a base não tem).
 * Cada lista some quando está vazia; sem nenhuma, nada aparece.
 */
import { useId, type ReactNode } from "react";
import type { KbCitationUse, KbDoc, KbNote, KbSection } from "../../api/kb";
import { Icon, type IconName } from "../../desktop/Icon";
import { useT } from "../../i18n/react";
import { KindPill } from "../plan/KbSources";
import { useLabels as placesOf } from "../plan/kbSourcesModel";
import { kindIcon, statusText, STATUS_TONE } from "./knowledgeModel";

/** Pedaço da linha que cita: texto comum, o texto do link (o alvo some) ou código em linha. */
interface ContextPart {
  text: string;
  kind: "text" | "link" | "code";
}

/**
 * A linha do documento que cita, legível: o marcador de lista sai e cada `[texto](alvo)` vira o
 * texto, marcado como link. Só texto: nada do conteúdo vira HTML.
 */
export function contextParts(line: string): ContextPart[] {
  const clean = line.replace(/^\s*(?:[-*+]\s+|\d+[.)]\s+|>\s*)/, "");
  const parts: ContextPart[] = [];
  const re = /\[([^\]\n]+)\]\([^)\s]*(?:\s+"[^"]*")?\)|`([^`\n]+)`/g;
  let at = 0;
  for (let m = re.exec(clean); m !== null; m = re.exec(clean)) {
    if (m.index > at) parts.push({ text: clean.slice(at, m.index), kind: "text" });
    if (m[1] !== undefined) parts.push({ text: m[1], kind: "link" });
    else parts.push({ text: m[2] ?? "", kind: "code" });
    at = m.index + m[0].length;
  }
  if (at < clean.length) parts.push({ text: clean.slice(at), kind: "text" });
  return parts;
}

/**
 * Só uma página `http:` ou `https:` vira link que abre em outra aba; qualquer outro alvo (`file:`,
 * `data:`, `javascript:`, texto inválido) fica como texto parado, sem `href`.
 */
function webPage(target: string): boolean {
  try {
    const url = new URL(target);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}

function Group({ kind, icon, title, count, children }: { kind: string; icon: IconName; title: string; count: number; children: ReactNode }) {
  const headingId = useId();
  return (
    <section className="nd-kb-rel" data-kb-rel={kind} aria-labelledby={headingId}>
      <h3 id={headingId} className="nd-kb-rel-head">
        <Icon name={icon} size={14} className="nd-kb-rel-head-icon" />
        <span>{title}</span>
        <span className="nd-kb-rel-count">{count}</span>
      </h3>
      <ul className="nd-kb-rel-list">{children}</ul>
    </section>
  );
}

function DocStatus({ status }: { status: KbDoc["status"] }) {
  if (status === "ready") return null;
  return (
    <span className="nd-pill nd-pill-sm nd-pill-nowrap" data-tone={STATUS_TONE[status]}>
      {statusText(status)}
    </span>
  );
}

/** Os usos de uma seção do documento num plano. */
interface AnchorUses {
  anchor: string | null;
  uses: KbCitationUse[];
  binding: boolean;
}

function byAnchor(uses: readonly (KbCitationUse & { anchor: string | null })[]): AnchorUses[] {
  const out: AnchorUses[] = [];
  for (const use of uses) {
    const group = out.find((g) => g.anchor === use.anchor);
    if (group === undefined) out.push({ anchor: use.anchor, uses: [use], binding: use.binding });
    else {
      group.uses.push(use);
      group.binding ||= use.binding;
    }
  }
  return out;
}

interface RelationsProps {
  note: KbNote;
  docs: readonly KbDoc[];
  sections: readonly KbSection[];
  onOpenDoc: (doc: string) => void;
  onJump: (anchor: string) => void;
}

export function KbRelations({ note, docs, sections, onOpenDoc, onJump }: RelationsProps) {
  const { t, lang } = useT();
  const plans = note.used_in_plans ?? [];
  const citedBy = note.cited_by ?? [];
  const linksDocs = note.links_to?.docs ?? [];
  const outside = note.links_to?.outside ?? [];
  if (plans.length === 0 && citedBy.length === 0 && linksDocs.length + outside.length === 0) return null;
  const docOf = (id: string): KbDoc | undefined => docs.find((d) => d.id === id);
  const sectionTitle = (anchor: string | null): string => (anchor === null ? t("kb.picker.wholeDoc") : (sections.find((s) => s.anchor === anchor)?.title ?? anchor));

  return (
    <div className="nd-kb-rels" data-kb-rels="" data-pair={citedBy.length > 0 && linksDocs.length + outside.length > 0 ? "true" : undefined} aria-label={t("kb.rel.label")} role="group">
      {plans.length > 0 ? (
        <Group kind="plans" icon="plan" title={t("kb.rel.usedInPlans")} count={plans.length}>
          {plans.map((plan) => (
            <li key={plan.spec} className="nd-kb-rel-plan" data-kb-rel-plan={plan.spec}>
              <div className="nd-kb-rel-plan-head">
                <span className="nd-kb-rel-plan-title">{plan.title ?? plan.spec}</span>
                {plan.title !== null ? <span className="nd-kb-rel-plan-id">{plan.spec}</span> : null}
                {plan.frozen ? (
                  <span className="nd-pill nd-pill-sm nd-pill-nowrap" data-tone="green" title={t("kb.rel.approvedHint")}>
                    <Icon name="check" size={14} />
                    {t("kb.rel.approved")}
                  </span>
                ) : null}
              </div>
              <ul className="nd-kb-rel-uses">
                {byAnchor(plan.uses).map((group) => {
                  const live = group.uses.filter((u) => u.in !== "frozen");
                  const where = live.length > 0 ? placesOf(live, lang).join(", ") : t("kb.rel.frozenOnly");
                  const canJump = group.anchor !== null && sections.some((s) => s.anchor === group.anchor);
                  const body = (
                    <>
                      <span className="nd-kb-rel-use-section">
                        {group.anchor !== null ? (
                          <span className="nd-kb-rel-hash" aria-hidden="true">
                            #
                          </span>
                        ) : null}
                        {sectionTitle(group.anchor)}
                      </span>
                      <span className="nd-kb-rel-use-where">{where}</span>
                      <KindPill binding={group.binding} />
                    </>
                  );
                  return (
                    <li key={group.anchor ?? ""}>
                      {canJump && group.anchor !== null ? (
                        <button type="button" className="nd-kb-rel-item nd-kb-rel-use" aria-label={t("kb.outline.jump", { section: sectionTitle(group.anchor) })} onClick={() => onJump(group.anchor ?? "")}>
                          {body}
                        </button>
                      ) : (
                        <span className="nd-kb-rel-item nd-kb-rel-use" data-static="true">
                          {body}
                        </span>
                      )}
                    </li>
                  );
                })}
              </ul>
            </li>
          ))}
        </Group>
      ) : null}
      {citedBy.length > 0 ? (
        <Group kind="cited" icon="arrow-left" title={t("kb.rel.citedBy")} count={citedBy.length}>
          {citedBy.map((item) => {
            const kbDoc = docOf(item.id);
            return (
              <li key={item.id}>
                <button type="button" className="nd-kb-rel-item nd-kb-rel-doc" data-kb-rel-doc={item.id} aria-label={t("kb.rel.open", { doc: item.title })} onClick={() => onOpenDoc(item.id)}>
                  <span className="nd-kb-rel-doc-line">
                    <Icon name={kbDoc === undefined ? "file" : kindIcon(kbDoc)} size={14} className="nd-kb-rel-doc-icon" />
                    <span className="nd-kb-rel-doc-title">{item.title}</span>
                    <DocStatus status={item.status} />
                    <Icon name="chevron-right" size={14} className="nd-kb-rel-go" />
                  </span>
                  {item.context !== null ? (
                    <span className="nd-kb-rel-context">
                      {contextParts(item.context).map((part, i) =>
                        part.kind === "link" ? (
                          <span key={i} className="nd-kb-rel-context-link">
                            {part.text}
                          </span>
                        ) : part.kind === "code" ? (
                          <code key={i} className="nd-md-code">
                            {part.text}
                          </code>
                        ) : (
                          part.text
                        ),
                      )}
                    </span>
                  ) : null}
                </button>
              </li>
            );
          })}
        </Group>
      ) : null}
      {linksDocs.length + outside.length > 0 ? (
        <Group kind="links" icon="link" title={t("kb.rel.linksTo")} count={linksDocs.length + outside.length}>
          {linksDocs.map((item) => {
            const kbDoc = docOf(item.id);
            return (
              <li key={item.id}>
                <button type="button" className="nd-kb-rel-item nd-kb-rel-doc" data-kb-rel-doc={item.id} aria-label={t("kb.rel.open", { doc: item.title })} onClick={() => onOpenDoc(item.id)}>
                  <span className="nd-kb-rel-doc-line">
                    <Icon name={kbDoc === undefined ? "file" : kindIcon(kbDoc)} size={14} className="nd-kb-rel-doc-icon" />
                    <span className="nd-kb-rel-doc-title">{item.title}</span>
                    <DocStatus status={item.status} />
                    <Icon name="chevron-right" size={14} className="nd-kb-rel-go" />
                  </span>
                </button>
              </li>
            );
          })}
          {outside.map((item) =>
            item.kind === "url" && webPage(item.target) ? (
              <li key={`u:${item.target}`}>
                <a className="nd-kb-rel-item nd-kb-rel-doc" href={item.target} target="_blank" rel="noreferrer noopener" aria-label={t("kb.rel.openLink", { target: item.target })}>
                  <span className="nd-kb-rel-doc-line">
                    <Icon name="globe" size={14} className="nd-kb-rel-doc-icon" data-outside="" />
                    <span className="nd-kb-rel-doc-title nd-kb-rel-url">{item.target.replace(/^https?:\/\/(www\.)?/, "")}</span>
                    <Icon name="external" size={14} className="nd-kb-rel-go" />
                  </span>
                </a>
              </li>
            ) : (
              <li key={`${item.kind}:${item.target}`}>
                <span className="nd-kb-rel-item nd-kb-rel-doc" data-static="true" title={item.kind === "file" ? t("kb.rel.missingFile") : undefined}>
                  <span className="nd-kb-rel-doc-line">
                    <Icon name="file" size={14} className="nd-kb-rel-doc-icon" data-outside="" />
                    <span className="nd-kb-rel-doc-title nd-kb-rel-file">{item.target}</span>
                    <span className="nd-pill nd-pill-sm nd-pill-nowrap" data-tone="amber">
                      {t("kb.rel.outside")}
                    </span>
                  </span>
                </span>
              </li>
            ),
          )}
        </Group>
      ) : null}
    </div>
  );
}
