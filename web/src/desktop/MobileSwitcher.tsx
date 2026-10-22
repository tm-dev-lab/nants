/*
 * Alternador do modo celular (spec 093, decisão 17): lista vertical com todas as janelas do
 * espaço, sem sobreposição (a spec prevalece sobre o mockup, que empilha cartões). Cada item
 * mostra o ícone, o título, o subtítulo vivo e o selo; tocar abre a janela em tela cheia.
 */
import { memo } from "react";
import { useT } from "../i18n/react";
import type { DockBadge } from "./Dock";
import { BADGE_MAX } from "./Dock";
import { Icon } from "./Icon";
import { WINDOW_META } from "./windowMeta";
import type { WindowKind } from "./windows";

interface MobileSwitcherProps {
  kinds: readonly WindowKind[];
  /** Janelas já abertas (montadas) no celular ou no desktop. */
  open: ReadonlySet<WindowKind>;
  subtitles?: Partial<Record<WindowKind, string>>;
  badges?: Partial<Record<WindowKind, DockBadge>>;
  /**
   * Janela da vista atual (a última mostrada em tela cheia antes do Alternador): só ela ganha o
   * sinal de selecionado (094/T08, A-12). As outras abertas ficam com a hairline neutra.
   */
  current?: WindowKind | null;
  onOpen: (kind: WindowKind) => void;
}

const switcherItemDetailsId = (kind: WindowKind): string => `switcher-${kind}-details`;

function MobileSwitcherView({ kinds, open, subtitles, badges, current = null, onOpen }: MobileSwitcherProps) {
  const { t } = useT();
  return (
    <section className="nd-switcher" aria-labelledby="nd-switcher-title" data-switcher="">
      <h2 id="nd-switcher-title" className="nd-switcher-title">
        {t("mobile.windows")}
      </h2>
      <ul className="nd-switcher-list">
        {kinds.map((kind) => {
          const meta = WINDOW_META[kind];
          const subtitle = subtitles?.[kind];
          const badge = badges?.[kind];
          const showBadge = badge !== undefined && badge.count > 0;
          const details = [open.has(kind) ? t("dock.state.open") : undefined, subtitle, showBadge ? badge.label : undefined].filter(
            (part): part is string => part !== undefined && part !== "",
          );
          return (
            <li key={kind} className="nd-switcher-entry">
              <button
                type="button"
                className="nd-switcher-item"
                data-kind={kind}
                data-open={open.has(kind) ? "true" : "false"}
                aria-current={kind === current ? "true" : undefined}
                aria-describedby={details.length > 0 ? switcherItemDetailsId(kind) : undefined}
                onClick={() => onOpen(kind)}
              >
                <span className="nd-switcher-icon">
                  <Icon name={meta.icon} size={20} />
                </span>
                <span className="nd-switcher-text">
                  <span className="nd-switcher-name">{meta.title}</span>
                  {details.length > 0 ? (
                    <span id={switcherItemDetailsId(kind)} className="nd-switcher-sub">
                      {details.join(" · ")}
                    </span>
                  ) : null}
                </span>
                {showBadge ? (
                  <span className="nd-badge nd-switcher-badge" data-badge="" aria-hidden="true">
                    {Math.min(badge.count, BADGE_MAX)}
                  </span>
                ) : null}
                <Icon name="chevron-right" size={16} className="nd-switcher-go" />
              </button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

export const MobileSwitcher = memo(MobileSwitcherView);
