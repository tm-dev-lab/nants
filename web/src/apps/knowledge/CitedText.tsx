/*
 * Texto do plano com as citações `kb:<id>#<seção>` como links (spec 134 T06, critério 2): cada
 * citação vira um botão com o título do documento (da lista da base) e a seção, que abre o
 * Conhecimento no documento e na seção citada. O texto em volta segue como texto puro.
 */
import "./knowledge.css";
import { Icon } from "../../desktop/Icon";
import { useShell } from "../../desktop/shellContext";
import { useT } from "../../i18n/react";
import { useKb } from "./kbStore";
import { splitCitations } from "./knowledgeModel";
import { requestOpenKbDoc } from "./openRequest";

export function CitedText({ projectId, text }: { projectId: string; text: string }) {
  const { t } = useT();
  const shell = useShell();
  const parts = splitCitations(text);
  const hasCitation = parts.some((p) => p.kind === "cite");
  const kb = useKb(hasCitation ? projectId : null);
  if (!hasCitation) return <>{text}</>;
  const docs = kb?.status === "ok" ? kb.list.docs : [];
  return (
    <>
      {parts.map((part, i) => {
        if (part.kind === "text") return <span key={i}>{part.text}</span>;
        const { cite } = part;
        const title = docs.find((d) => d.id === cite.doc)?.title ?? cite.doc;
        const label = cite.section === null ? t("plan.kb.open", { doc: title }) : t("plan.kb.openSection", { doc: title, section: cite.section });
        return (
          <button
            key={i}
            type="button"
            className="nd-link-btn nd-kb-cite"
            data-kb-cite={cite.text}
            aria-label={label}
            title={cite.text}
            onClick={() => {
              requestOpenKbDoc(projectId, cite.doc, cite.section);
              shell.openWindow("knowledge");
            }}
          >
            <Icon name="knowledge" size={14} />
            <span>{title}</span>
            {cite.section !== null ? <span className="nd-kb-cite-section">#{cite.section}</span> : null}
          </button>
        );
      })}
    </>
  );
}
