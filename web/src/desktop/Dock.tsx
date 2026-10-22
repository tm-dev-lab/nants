/*
 * Dock (spec 093, decisão 9): os ícones das janelas do espaço, com indicador de aberta,
 * minimizada ou em foco e um selo por janela (pendências, "N novas"). A dica mostra o título, o
 * subtítulo vivo e o estado (também como `aria-describedby`). Clique: abre, foca, restaura ou
 * minimiza a que está em foco (decidido pelo Desktop). Teclado: Tab, setas, Home/End e Alt+1…9.
 *
 * Ícones NEUTROS (095/T02, decisão 9; `reference/Main.dc.html`): a cor vem só do estado (`--ink`
 * com a janela aberta, `--muted` fechada); o tom por aplicativo fica no ícone da barra de título.
 */
import { memo, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { localized } from "../i18n";
import { useT } from "../i18n/react";
import { dockButtonId } from "./focus";
import { Icon } from "./Icon";
import { dockSlotAria } from "./shortcuts";
import { WINDOW_META } from "./windowMeta";
import { isShown, topWindow, type Layout, type WindowKind } from "./windows";

export interface DockBadge {
  /** Número exibido (até 99). */
  count: number;
  /** Texto acessível (ex.: "2 pendências"). */
  label: string;
}

type DockItemState = "closed" | "open" | "minimized" | "focused";

const STATE_LABELS: Readonly<Record<DockItemState, string>> = localized({
  closed: "dock.state.closed",
  open: "dock.state.open",
  minimized: "dock.state.minimized",
  focused: "dock.state.focused",
});

/** Maior número desenhado no selo (o texto acessível traz o valor real). */
export const BADGE_MAX = 99;

export function dockItemState(layout: Layout, kind: WindowKind, top: WindowKind | null = topWindow(layout)): DockItemState {
  const win = layout.windows[kind];
  if (win === undefined || !win.open) return "closed";
  if (win.mode === "minimized") return "minimized";
  if (isShown(win) && top === kind) return "focused";
  return "open";
}

export const dockTipId = (kind: WindowKind): string => `dock-${kind}-tip`;

interface DockProps {
  kinds: readonly WindowKind[];
  layout: Layout;
  subtitles?: Partial<Record<WindowKind, string>>;
  badges?: Partial<Record<WindowKind, DockBadge>>;
  onActivate: (kind: WindowKind) => void;
}

function DockView({ kinds, layout, subtitles, badges, onActivate }: DockProps) {
  const { t } = useT();
  const top = topWindow(layout);

  const onKeyDown = (event: ReactKeyboardEvent<HTMLUListElement>): void => {
    const buttons = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>(".nd-dock-btn"));
    const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (index === -1) return;
    let next: number | null = null;
    if (event.key === "ArrowRight") next = (index + 1) % buttons.length;
    else if (event.key === "ArrowLeft") next = (index - 1 + buttons.length) % buttons.length;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = buttons.length - 1;
    if (next === null) return;
    event.preventDefault();
    buttons[next]?.focus();
  };

  return (
    <nav className="nd-dock nd-chrome" aria-label={t("dock.label")}>
      <ul className="nd-dock-list" onKeyDown={onKeyDown}>
        {kinds.map((kind, index) => {
          const meta = WINDOW_META[kind];
          const state = dockItemState(layout, kind, top);
          const subtitle = subtitles?.[kind];
          const badge = badges?.[kind];
          const showBadge = badge !== undefined && badge.count > 0;
          const details = [STATE_LABELS[state], subtitle, showBadge ? badge.label : undefined].filter(
            (part): part is string => part !== undefined && part !== "",
          );
          return (
            <li key={kind} className="nd-dock-item">
              <button
                type="button"
                id={dockButtonId(kind)}
                className="nd-dock-btn"
                data-kind={kind}
                data-state={state}
                aria-label={meta.title}
                aria-describedby={dockTipId(kind)}
                aria-keyshortcuts={dockSlotAria(index + 1)}
                onClick={() => onActivate(kind)}
              >
                <Icon name={meta.icon} size={20} />
                {state !== "closed" ? <span className="nd-dock-indicator" data-indicator={state} aria-hidden="true" /> : null}
                {showBadge ? (
                  <span className="nd-badge" data-badge="" aria-hidden="true">
                    {Math.min(badge.count, BADGE_MAX)}
                  </span>
                ) : null}
              </button>
              <span id={dockTipId(kind)} role="tooltip" className="nd-tip nd-overlay">
                <span className="nd-tip-title">{meta.title}</span>
                {details.map((part) => (
                  <span key={part} className="nd-tip-sub">
                    {" · "}
                    {part}
                  </span>
                ))}
              </span>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}

export const Dock = memo(DockView);
