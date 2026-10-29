/*
 * Texto do plano com as citações `kb:<id>#<seção>` como links (spec 134 T06, critério 2): cada
 * citação vira um botão com o título do documento (da lista da base) e a seção, que abre o
 * Conhecimento no documento e na seção citada. O texto em volta segue como texto puro.
 *
 * Prévia (spec 136 T04, critério 4): no desktop, passar o mouse (ou focar pelo teclado) abre em
 * 250 ms o cartão com o trecho, e o clique abre a base; no celular, o toque abre a folha inferior,
 * que tem o "Abrir na base". A citação de um documento que saiu da base fica riscada.
 */
import "./knowledge.css";
import { useEffect, useRef, useState } from "react";
import type { KbDoc } from "../../api/kb";
import { Icon } from "../../desktop/Icon";
import { useShell } from "../../desktop/shellContext";
import { useMobileMode } from "../../desktop/viewport";
import { useT } from "../../i18n/react";
import { CiteCard, CiteSheet, type CitePreviewTarget } from "./CitePreview";
import { useKbSections } from "./kbCiteStore";
import { useKb } from "./kbStore";
import { splitCitations } from "./knowledgeModel";
import { requestOpenKbDoc } from "./openRequest";

/** Espera antes de abrir o cartão (critério 4: até 300 ms) e antes de fechá-lo ao sair. */
const OPEN_DELAY_MS = 250;
const CLOSE_DELAY_MS = 160;

function Citation({ target, text }: { target: CitePreviewTarget; text: string }) {
  const { t } = useT();
  const shell = useShell();
  const mobile = useMobileMode();
  const ref = useRef<HTMLButtonElement | null>(null);
  const [open, setOpen] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const clear = (): void => {
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = null;
  };
  useEffect(() => clear, []);
  const later = (next: boolean, ms: number): void => {
    clear();
    timer.current = setTimeout(() => setOpen(next), ms);
  };
  const openInBase = (): void => {
    clear();
    setOpen(false);
    requestOpenKbDoc(target.projectId, target.doc, target.section);
    shell.openWindow("knowledge");
  };
  const removed = target.kbDoc === null;
  // A etiqueta mostra o título da seção, como na caixa da Conversa (T06); sem o sumário, a âncora.
  const outline = useKbSections(target.projectId, target.section !== null && !removed && target.kbDoc?.id !== "" ? target.doc : null);
  const sectionTitle =
    target.section === null ? null : outline.status === "ok" ? (outline.sections.find((s) => s.anchor === target.section)?.title ?? target.section) : target.section;
  const label =
    sectionTitle === null ? t("plan.kb.open", { doc: target.title }) : t("plan.kb.openSection", { doc: target.title, section: sectionTitle });
  return (
    <>
      <button
        ref={ref}
        type="button"
        className="nd-link-btn nd-kb-cite"
        data-kb-cite={text}
        data-removed={removed ? "true" : undefined}
        aria-label={label}
        aria-expanded={open}
        aria-haspopup="dialog"
        onClick={() => {
          if (mobile || removed) {
            clear();
            setOpen(true);
            return;
          }
          openInBase();
        }}
        onMouseEnter={() => {
          if (!mobile) later(true, OPEN_DELAY_MS);
        }}
        onMouseLeave={() => {
          if (!mobile) later(false, CLOSE_DELAY_MS);
        }}
        onFocus={() => {
          if (!mobile) later(true, OPEN_DELAY_MS);
        }}
        onBlur={(event) => {
          if (mobile) return;
          const next = event.relatedTarget;
          if (next instanceof Node && document.querySelector("[data-kb-card]")?.contains(next) === true) return;
          later(false, CLOSE_DELAY_MS);
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape" && open) {
            event.preventDefault();
            event.stopPropagation();
            clear();
            setOpen(false);
          }
        }}
      >
        <Icon name="knowledge" size={14} />
        <span>{target.title}</span>
        {sectionTitle !== null ? <span className="nd-kb-cite-section">#{sectionTitle}</span> : null}
      </button>
      {open && !mobile ? (
        <CiteCard
          target={target}
          anchor={ref}
          onOpen={openInBase}
          onHover={(inside) => (inside ? clear() : later(false, CLOSE_DELAY_MS))}
          onClose={(returnFocus) => {
            clear();
            setOpen(false);
            if (returnFocus) ref.current?.focus({ preventScroll: true });
          }}
        />
      ) : null}
      {open && mobile ? (
        <CiteSheet
          target={target}
          onOpen={openInBase}
          onClose={() => {
            setOpen(false);
            ref.current?.focus({ preventScroll: true });
          }}
        />
      ) : null}
    </>
  );
}

export function CitedText({ projectId, text }: { projectId: string; text: string }) {
  const parts = splitCitations(text);
  const hasCitation = parts.some((p) => p.kind === "cite");
  const kb = useKb(hasCitation ? projectId : null);
  if (!hasCitation) return <>{text}</>;
  const loaded = kb?.status === "ok";
  const docs = loaded ? kb.list.docs : [];
  return (
    <>
      {parts.map((part, i) => {
        if (part.kind === "text") return <span key={i}>{part.text}</span>;
        const { cite } = part;
        const kbDoc = docs.find((d) => d.id === cite.doc) ?? null;
        // Sem a lista ainda, a citação não aparece como removida.
        const target: CitePreviewTarget = {
          projectId,
          doc: cite.doc,
          section: cite.section,
          kbDoc: loaded ? kbDoc : (kbDoc ?? PENDING_DOC),
          title: kbDoc?.title ?? cite.doc,
        };
        return <Citation key={i} target={target} text={cite.text} />;
      })}
    </>
  );
}

/** Documento provisório enquanto a lista da base carrega (não marca a citação como removida). */
const PENDING_DOC: KbDoc = {
  id: "",
  title: "",
  kind: "markdown",
  source: "file",
  file_name: null,
  source_url: null,
  size: 0,
  added_at: "",
  fetched_at: null,
  status: "pending",
  pages: null,
  description: "",
  links_out: [],
};
