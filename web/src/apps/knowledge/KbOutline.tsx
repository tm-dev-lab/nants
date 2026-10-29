/*
 * Sumário do documento na prévia do Conhecimento (spec 136 T07, G8), como o "outline" do
 * Obsidian: os títulos em níveis, a seção em leitura marcada, e um toque pula até a seção e a
 * destaca. Fixo ao lado da nota na janela larga; recolhido no topo da nota na estreita e no
 * celular, abrindo e fechando sem empurrar o resto de repente.
 */
import { useId, useState } from "react";
import type { KbSection } from "../../api/kb";
import { Icon } from "../../desktop/Icon";
import { useT } from "../../i18n/react";

interface OutlineProps {
  sections: readonly KbSection[];
  /** Seção em leitura (a última cujo título já passou do topo da nota). */
  current: string | null;
  onJump: (anchor: string) => void;
  variant: "side" | "collapsed";
}

function OutlineList({ sections, current, onJump, id }: Omit<OutlineProps, "variant"> & { id?: string }) {
  const { t } = useT();
  const top = Math.min(...sections.map((s) => s.level), 6);
  return (
    <ul id={id} className="nd-kb-outline-list">
      {sections.map((section) => {
        const depth = Math.min(Math.max(0, section.level - top), 3);
        return (
          <li key={section.anchor}>
            <button
              type="button"
              className="nd-kb-outline-item"
              data-depth={depth}
              aria-current={section.anchor === current ? "location" : undefined}
              title={t("kb.outline.jump", { section: section.title })}
              onClick={() => onJump(section.anchor)}
            >
              <span className="nd-kb-outline-text">{section.title}</span>
            </button>
          </li>
        );
      })}
    </ul>
  );
}

export function KbOutline({ sections, current, onJump, variant }: OutlineProps) {
  const { t, tp } = useT();
  const [open, setOpen] = useState(false);
  const listId = useId();
  const headingId = useId();
  if (variant === "side") {
    return (
      <nav className="nd-kb-outline" data-variant="side" aria-labelledby={headingId} data-kb-outline="">
        <h3 id={headingId} className="nd-section-label nd-kb-outline-head">
          {t("kb.outline.title")}
        </h3>
        <OutlineList sections={sections} current={current} onJump={onJump} />
      </nav>
    );
  }
  const here = sections.find((s) => s.anchor === current) ?? null;
  return (
    <nav className="nd-kb-outline" data-variant="collapsed" data-open={open ? "true" : undefined} aria-label={t("kb.outline.title")} data-kb-outline="">
      <button type="button" className="nd-disclosure-btn nd-kb-outline-toggle" aria-expanded={open} aria-controls={listId} onClick={() => setOpen((v) => !v)}>
        <Icon name="outline" size={14} className="nd-kb-outline-icon" />
        <span className="nd-kb-outline-toggle-title">{t("kb.outline.title")}</span>
        <span className="nd-kb-outline-toggle-meta">{here !== null && !open ? here.title : tp("kb.outline.sections", sections.length)}</span>
        <Icon name="chevron-down" size={14} className="nd-kb-outline-chevron" />
      </button>
      <div className="nd-kb-outline-drawer" hidden={!open}>
        <OutlineList
          id={listId}
          sections={sections}
          current={current}
          onJump={(anchor) => {
            setOpen(false);
            onJump(anchor);
          }}
        />
      </div>
    </nav>
  );
}
