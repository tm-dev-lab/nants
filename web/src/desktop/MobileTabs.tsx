/*
 * Abas do modo celular (spec 093, decisão 17), na barra de baixo do celular: as janelas da categoria da fase
 * e o Alternador (no meio, em destaque). Alvos com ≥ 44 px; somem com o teclado virtual aberto
 * (`.nd-keyboard-hide`). A aba atual tem `aria-current="page"`.
 */
import { memo } from "react";
import { BADGE_MAX, type AppBadge } from "./appState";
import { Icon, type IconName } from "./Icon";
import { useT } from "../i18n/react";
import { MOBILE_VIEW_LABELS, viewWindow, type MobileView } from "./mobile";
import { WINDOW_META } from "./windowMeta";
import type { WindowKind } from "./windows";

export const mobileTabId = (view: MobileView): string => `mobile-tab-${view}`;

/** Rótulo e ícone de uma vista do celular. */
function mobileViewMeta(view: MobileView): { label: string; icon: IconName } {
  const kind = viewWindow(view);
  if (kind !== null) return { label: WINDOW_META[kind].title, icon: WINDOW_META[kind].icon };
  return view === "home" ? { label: MOBILE_VIEW_LABELS.home, icon: "home" } : { label: MOBILE_VIEW_LABELS.switcher, icon: "switcher" };
}

interface MobileTabsProps {
  tabs: readonly MobileView[];
  view: MobileView;
  badges?: Partial<Record<WindowKind, AppBadge>>;
  onSelect: (view: MobileView) => void;
}

function MobileTabsView({ tabs, view, badges, onSelect }: MobileTabsProps) {
  const { t } = useT();
  return (
    <nav className="nd-mobile-tabs nd-chrome nd-keyboard-hide" aria-label={t("mobile.tabs")} data-mobile-tabs="">
      <ul className="nd-mobile-tab-list">
        {tabs.map((tab) => {
          const meta = mobileViewMeta(tab);
          const kind = viewWindow(tab);
          const badge = kind === null ? undefined : badges?.[kind];
          const showBadge = badge !== undefined && badge.count > 0;
          const current = tab === view;
          return (
            <li key={tab} className="nd-mobile-tab-item">
              <button
                type="button"
                id={mobileTabId(tab)}
                className="nd-mobile-tab"
                data-tab={tab}
                data-variant={tab === "switcher" ? "switcher" : undefined}
                aria-current={current ? "page" : undefined}
                onClick={() => onSelect(tab)}
              >
                <span className="nd-mobile-tab-icon">
                  <Icon name={meta.icon} size={20} />
                  {showBadge ? (
                    <span className="nd-badge" data-badge="" aria-hidden="true">
                      {Math.min(badge.count, BADGE_MAX)}
                    </span>
                  ) : null}
                </span>
                <span className="nd-mobile-tab-label">{meta.label}</span>
                {showBadge ? <span className="nd-sr-only">{`, ${badge.label}`}</span> : null}
              </button>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}

export const MobileTabs = memo(MobileTabsView);
