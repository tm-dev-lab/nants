/*
 * Menu da janela (spec 093, decisão 4): a alternativa de ponteiro único (WCAG 2.5.7). Abre pelo
 * botão da barra ou por Shift+F10/tecla Menu com foco na barra. Setas, Home e End navegam;
 * Enter/Espaço escolhem; Esc fecha e devolve o foco a quem abriu; Tab ou clique fora fecham.
 */
import { useEffect, useRef, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { localized } from "../i18n";
import { useT } from "../i18n/react";
import { Icon, type IconName } from "./Icon";
import { WINDOW_MENU, type WindowMenuItem } from "./windowMenu";

const GROUP_LABELS: Readonly<Record<WindowMenuItem["group"], string>> = localized({
  snap: "windowMenu.group.snap",
  size: "windowMenu.group.size",
  keyboard: "windowMenu.group.keyboard",
  window: "windowMenu.group.window",
});

const ITEM_ICON: Readonly<Partial<Record<string, IconName>>> = {
  maximize: "maximize",
  minimize: "minimize",
  close: "close",
  "keyboard-move": "keyboard",
  "keyboard-resize": "keyboard",
};

interface WindowMenuProps {
  /** Id do menu (o botão que o abre aponta para ele com `aria-controls`). */
  id: string;
  /** Rótulo do menu (ex.: "Menu da janela Conversa"). */
  label: string;
  maximized: boolean;
  style?: CSSProperties;
  onChoose: (item: WindowMenuItem) => void;
  /** Fechar; `returnFocus` = devolver o foco a quem abriu (Esc). */
  onClose: (returnFocus: boolean) => void;
  /** Quem abriu o menu (o botão alterna sozinho; clicar nele não conta como "fora"). */
  opener?: () => Element | null;
}

export function WindowMenu({ id, label, maximized, style, onChoose, onClose, opener }: WindowMenuProps) {
  const { t } = useT();
  const ref = useRef<HTMLDivElement | null>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  const openerRef = useRef(opener);
  openerRef.current = opener;

  useEffect(() => {
    ref.current?.querySelector<HTMLElement>("[role='menuitem']")?.focus({ preventScroll: true });
    const onPointerDown = (event: PointerEvent | MouseEvent): void => {
      const target = event.target;
      if (target instanceof Node && ref.current?.contains(target) === true) return;
      if (target instanceof Node && openerRef.current?.()?.contains(target) === true) return;
      onCloseRef.current(false);
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    return () => document.removeEventListener("pointerdown", onPointerDown, true);
  }, []);

  const items = (): HTMLElement[] => Array.from(ref.current?.querySelectorAll<HTMLElement>("[role='menuitem']") ?? []);

  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>): void => {
    const list = items();
    const index = list.indexOf(document.activeElement as HTMLElement);
    const move = (next: number): void => {
      event.preventDefault();
      list[(next + list.length) % list.length]?.focus({ preventScroll: true });
    };
    switch (event.key) {
      case "ArrowDown":
        move(index + 1);
        return;
      case "ArrowUp":
        move(index <= 0 ? list.length - 1 : index - 1);
        return;
      case "Home":
        move(0);
        return;
      case "End":
        move(list.length - 1);
        return;
      case "Escape":
        event.preventDefault();
        event.stopPropagation();
        onClose(true);
        return;
      case "Tab":
        onClose(false);
        return;
      default:
    }
  };

  return (
    <div
      ref={ref}
      id={id}
      role="menu"
      aria-label={label}
      className="nd-menu nd-overlay"
      style={style}
      onKeyDown={onKeyDown}
    >
      {GROUPS.map((group, groupIndex) => (
        <div key={group} role="group" aria-labelledby={`${id}-${group}`}>
          {groupIndex > 0 ? <div role="separator" className="nd-menu-sep" /> : null}
          <div id={`${id}-${group}`} className="nd-menu-label">
            {GROUP_LABELS[group]}
          </div>
          {WINDOW_MENU.filter((item) => item.group === group).map((item) => {
            const text = item.id === "maximize" && maximized ? t("windowControls.restore") : item.label;
            const icon = item.id === "maximize" && maximized ? "restore" : ITEM_ICON[item.id];
            return (
              <button
                key={item.id}
                type="button"
                role="menuitem"
                tabIndex={-1}
                data-command={item.id}
                className="nd-menu-item"
                onClick={() => onChoose(item)}
              >
                {icon !== undefined ? <Icon name={icon} size={14} /> : null}
                <span>{text}</span>
              </button>
            );
          })}
        </div>
      ))}
    </div>
  );
}

const GROUPS: readonly WindowMenuItem["group"][] = ["snap", "size", "keyboard", "window"];
