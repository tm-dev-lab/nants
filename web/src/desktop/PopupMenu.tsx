/*
 * Menu suspenso da barra superior (seletor de espaços, Layout): `role="menu"` na camada de
 * sobreposição, ancorado no botão que o abriu. Setas, Home/End e Enter navegam; Esc fecha e
 * devolve o foco ao botão (sem chegar aos atalhos do desktop); clique fora fecha.
 */
import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";
import { useDesktopLayer } from "./layer";

interface PopupMenuProps {
  label: string;
  anchor: RefObject<HTMLElement | null>;
  onClose: (returnFocus: boolean) => void;
  width?: number;
  children: ReactNode;
}

function items(menu: HTMLElement): HTMLElement[] {
  return Array.from(
    menu.querySelectorAll<HTMLElement>("[role='menuitem']:not([disabled]), [role='menuitemradio']:not([disabled])"),
  );
}

/** Posição do menu: logo abaixo do botão, sem sair da raiz. */
function placement(anchor: HTMLElement | null, layer: HTMLElement | null, width: number): { left: number; top: number } {
  if (anchor === null) return { left: 10, top: 48 };
  const a = anchor.getBoundingClientRect();
  const root = layer?.getBoundingClientRect();
  const originX = root?.left ?? 0;
  const originY = root?.top ?? 0;
  const maxLeft = Math.max(8, (root?.width ?? window.innerWidth) - width - 8);
  return { left: Math.min(Math.max(8, a.left - originX), maxLeft), top: a.bottom - originY + 6 };
}

export function PopupMenu({ label, anchor, onClose, width = 280, children }: PopupMenuProps) {
  const layer = useDesktopLayer();
  const ref = useRef<HTMLDivElement | null>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  const [pos, setPos] = useState(() => placement(anchor.current, layer, width));

  useLayoutEffect(() => {
    setPos(placement(anchor.current, layer, width));
  }, [anchor, layer, width]);

  useEffect(() => {
    const menu = ref.current;
    if (menu !== null) items(menu)[0]?.focus({ preventScroll: true });
    const onPointer = (event: PointerEvent | MouseEvent): void => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (ref.current?.contains(target) === true || anchor.current?.contains(target) === true) return;
      closeRef.current(false);
    };
    document.addEventListener("mousedown", onPointer);
    return () => document.removeEventListener("mousedown", onPointer);
  }, [anchor, layer]);

  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>): void => {
    const menu = ref.current;
    if (menu === null) return;
    const list = items(menu);
    const index = list.findIndex((el) => el === document.activeElement);
    const go = (i: number): void => {
      event.preventDefault();
      list[(i + list.length) % list.length]?.focus();
    };
    switch (event.key) {
      case "Escape":
        event.preventDefault();
        event.stopPropagation();
        closeRef.current(true);
        return;
      case "Tab":
        // Fecha e o foco volta ao botão que abriu o menu (não se perde atrás da camada).
        event.preventDefault();
        closeRef.current(true);
        return;
      case "ArrowDown":
        go(index + 1);
        return;
      case "ArrowUp":
        go(index < 0 ? list.length - 1 : index - 1);
        return;
      case "Home":
        go(0);
        return;
      case "End":
        go(list.length - 1);
        return;
      default:
    }
  };

  const node = (
    <div
      ref={ref}
      role="menu"
      aria-label={label}
      className="nd-menu nd-overlay"
      style={{ left: pos.left, top: pos.top, width }}
      onKeyDown={onKeyDown}
    >
      {children}
    </div>
  );
  return layer === null ? node : createPortal(node, layer);
}
