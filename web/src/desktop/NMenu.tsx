/*
 * Menu do N (spec 137, `reference/menu-n.dc.html`): o botão N na barra de cima, no lugar da marca,
 * e o menu que ele abre. O `Desktop` desenha os dois e decide abrir, fechar e ativar.
 *
 * - Botão: o símbolo oficial (`NantsSymbol`), em relevo discreto; afunda com o menu aberto. Um
 *   ponto rosa marca selo em algum app com o menu fechado (só os selos dos apps; a
 *   atenção fica no sino). `aria-expanded`, `aria-controls`, `aria-keyshortcuts` (Alt+0).
 * - Menu: o símbolo, o nome, o slogan logo depois (regra de identidade) e a versão; a busca, que
 *   filtra a grade (setas movem o destaque, Enter abre); a grade dos apps com os estados de cada um
 *   (fechado, aberto com o ponto, minimizado, em foco), os selos e a dica no `aria-describedby`;
 *   a linha das janelas abertas e minimizadas; o rodapé dos atalhos.
 * - Modal (`aria-modal`): o Tab fica preso no painel e o próprio menu trata o Esc, o Alt+0 e o
 *   Alt+1…9 (o `Desktop` não age com um modal aberto). Clique fora fecha.
 * - Fechando (`closing`): o painel continua desenhado, inerte e fora da árvore acessível, só
 *   durante a animação de subida; o `Desktop` o remove no fim (nunca depende de `animationend`).
 */
import { useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type RefObject } from "react";
import { localized, tp } from "../i18n";
import { useT } from "../i18n/react";
import { BRAND_NAME, BRAND_SLOGAN } from "./brand";
import { appItemState, BADGE_MAX, type AppBadge } from "./appState";
import { N_BUTTON_ID } from "./focus";
import { trapTab } from "./focusTrap";
import { Icon } from "./Icon";
import { normalizeQuery } from "./launcher";
import { appSlotAria, matchShortcut, shortcutById } from "./shortcuts";
import { NantsSymbol } from "./topbar/BrandMark";
import { WINDOW_META } from "./windowMeta";
import { topWindow, WINDOW_KINDS, type Layout, type WindowKind } from "./windows";

/** Id do painel (alvo do `aria-controls` do botão). */
export const NMENU_ID = "nd-nmenu";

/** Colunas da grade (as setas para cima e para baixo andam uma linha). */
const COLUMNS = 4;

const STATE_LABELS = localized({
  closed: "windowState.closed",
  open: "windowState.open",
  minimized: "windowState.minimized",
  focused: "windowState.focused",
});

const tileId = (kind: WindowKind): string => `nmenu-${kind}`;
const tipId = (kind: WindowKind): string => `nmenu-${kind}-tip`;

/** Soma dos selos dos apps da grade: o que o ponto do N anuncia. */
export function pendingCount(kinds: readonly WindowKind[], badges: Partial<Record<WindowKind, AppBadge>>): number {
  let total = 0;
  for (const kind of kinds) {
    const count = badges[kind]?.count ?? 0;
    if (count > 0) total += count;
  }
  return total;
}

interface NButtonProps {
  buttonRef: RefObject<HTMLButtonElement | null>;
  open: boolean;
  /** Selos somados dos apps (0: sem ponto). */
  pending: number;
  onToggle: () => void;
}

export function NButton({ buttonRef, open, pending, onToggle }: NButtonProps) {
  const { t } = useT();
  const label = pending > 0 ? tp("nmenu.buttonPending", pending) : t("nmenu.button");
  return (
    <button
      ref={buttonRef}
      id={N_BUTTON_ID}
      type="button"
      className="nd-nbtn"
      data-nbutton=""
      aria-label={label}
      aria-haspopup="dialog"
      aria-expanded={open}
      aria-controls={open ? NMENU_ID : undefined}
      aria-keyshortcuts={shortcutById("nMenu")?.aria}
      onClick={onToggle}
    >
      <NantsSymbol size={22} />
      {pending > 0 && !open ? <span className="nd-nbtn-dot" data-nbutton-dot="" aria-hidden="true" /> : null}
    </button>
  );
}

/** Como o menu fechou: pelo teclado ou pelo véu, o foco volta ao N. */
export type NMenuCloseReason = "key" | "scrim" | "outside";

interface NMenuProps {
  kinds: readonly WindowKind[];
  layout: Layout;
  subtitles: Partial<Record<WindowKind, string>>;
  badges: Partial<Record<WindowKind, AppBadge>>;
  version?: string;
  /** Subindo (animação de saída): inerte e fora da árvore acessível. */
  closing: boolean;
  panelRef: RefObject<HTMLDivElement | null>;
  /** O botão N: o clique nele não conta como "fora". */
  buttonRef: RefObject<HTMLButtonElement | null>;
  /** Item da grade (clique ou Enter): abre, foca, restaura ou minimiza a que está em foco. */
  onActivate: (kind: WindowKind) => void;
  /** Janela aberta (linha das abertas) ou Alt+1…9: abre, foca ou restaura, sem minimizar. */
  onShow: (kind: WindowKind) => void;
  onClose: (reason: NMenuCloseReason) => void;
}

export function NMenu({ kinds, layout, subtitles, badges, version, closing, panelRef, buttonRef, onActivate, onShow, onClose }: NMenuProps) {
  const { t } = useT();
  const [query, setQuery] = useState("");
  const [highlight, setHighlight] = useState(-1);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const layerRef = useRef<HTMLDivElement | null>(null);
  const top = topWindow(layout);

  const visible = useMemo(() => {
    const words = normalizeQuery(query).split(/\s+/).filter((w) => w !== "");
    if (words.length === 0) return [...kinds];
    return kinds.filter((kind) => {
      const title = normalizeQuery(WINDOW_META[kind].title);
      return words.every((w) => title.includes(w));
    });
    // `t`: o título é lido no idioma ativo.
  }, [kinds, query, t]);
  const current = visible.length === 0 ? -1 : highlight >= 0 ? Math.min(highlight, visible.length - 1) : query.trim() !== "" ? 0 : -1;
  const openKinds = useMemo(() => {
    const order = [...kinds, ...WINDOW_KINDS.filter((k) => !kinds.includes(k))];
    return order.filter((kind) => layout.windows[kind]?.open === true);
  }, [kinds, layout]);

  // Ao abrir (e ao reabrir durante a subida), a busca recebe o foco.
  useEffect(() => {
    if (closing) return;
    inputRef.current?.focus({ preventScroll: true });
  }, [closing]);

  // Esc, Alt+0 e Alt+1…9 com o menu aberto, de onde estiver o foco.
  const slotRef = useRef({ kinds, onShow, onClose });
  slotRef.current = { kinds, onShow, onClose };
  useEffect(() => {
    if (closing) return undefined;
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.defaultPrevented) return;
      const { kinds: order, onShow: show, onClose: close } = slotRef.current;
      if (event.key === "Escape") {
        event.preventDefault();
        close("key");
        return;
      }
      if (!event.altKey) return;
      const match = matchShortcut(event, { typing: false, onDesktop: true, onTitleBar: false });
      if (match?.id === "nMenu") {
        event.preventDefault();
        close("key");
      } else if (match?.id === "appSlot") {
        const kind = order[(match.slot ?? 0) - 1];
        if (kind === undefined) return;
        event.preventDefault();
        show(kind);
      }
    };
    // Clique fora do menu e do N fecha (o véu tem o próprio tratamento).
    const onPointerDown = (event: Event): void => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (layerRef.current?.contains(target) === true || buttonRef.current?.contains(target) === true) return;
      slotRef.current.onClose("outside");
    };
    window.addEventListener("keydown", onKeyDown);
    document.addEventListener("pointerdown", onPointerDown, true);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("pointerdown", onPointerDown, true);
    };
  }, [closing, buttonRef]);

  const onSearchKeyDown = (event: ReactKeyboardEvent<HTMLInputElement>): void => {
    const last = visible.length - 1;
    if (last < 0) return;
    const from = current < 0 ? -1 : current;
    let next: number | null = null;
    switch (event.key) {
      case "ArrowRight":
        next = from < 0 ? 0 : Math.min(last, from + 1);
        break;
      case "ArrowLeft":
        next = from < 0 ? 0 : Math.max(0, from - 1);
        break;
      case "ArrowDown":
        next = from < 0 ? 0 : Math.min(last, from + COLUMNS);
        break;
      case "ArrowUp":
        next = from < 0 ? 0 : Math.max(0, from - COLUMNS);
        break;
      case "Enter": {
        const kind = visible[from < 0 ? 0 : from];
        if (kind === undefined) return;
        event.preventDefault();
        onActivate(kind);
        return;
      }
      default:
        return;
    }
    event.preventDefault();
    setHighlight(next);
  };

  // Teclado na grade (foco num item): setas e Home/End.
  const onGridKeyDown = (event: ReactKeyboardEvent<HTMLUListElement>): void => {
    const buttons = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>(".nd-nmenu-tile"));
    const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (index === -1) return;
    const last = buttons.length - 1;
    let next: number | null = null;
    if (event.key === "ArrowRight") next = Math.min(last, index + 1);
    else if (event.key === "ArrowLeft") next = Math.max(0, index - 1);
    else if (event.key === "ArrowDown") next = Math.min(last, index + COLUMNS);
    else if (event.key === "ArrowUp") next = Math.max(0, index - COLUMNS);
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = last;
    if (next === null) return;
    event.preventDefault();
    buttons[next]?.focus();
  };

  const onPanelKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>): void => {
    trapTab(event, panelRef.current);
  };

  const highlighted = current >= 0 ? visible[current] : undefined;

  return (
    <div ref={layerRef} className="nd-nmenu-layer" data-nmenu-layer="" data-state={closing ? "closing" : "open"}>
      <div className="nd-nmenu-scrim" data-nmenu-scrim="" aria-hidden="true" onMouseDown={() => onClose("scrim")} />
      <div
        ref={panelRef}
        id={NMENU_ID}
        role="dialog"
        aria-modal={closing ? undefined : true}
        aria-label={t("nmenu.button")}
        aria-hidden={closing ? true : undefined}
        inert={closing}
        className="nd-nmenu nd-overlay"
        data-nmenu=""
        onKeyDown={onPanelKeyDown}
      >
        <div className="nd-nmenu-head" data-nmenu-head="">
          <NantsSymbol size={26} />
          <span className="nd-nmenu-brand">
            <span className="nd-nmenu-name">{BRAND_NAME}</span>
            <span className="nd-nmenu-slogan" data-brand-slogan="">
              {BRAND_SLOGAN}
            </span>
          </span>
          {version !== undefined && version !== "" ? (
            <span className="nd-nmenu-version" data-nmenu-version="" aria-label={t("nmenu.version", { version })}>
              {version}
            </span>
          ) : null}
        </div>
        <label className="nd-nmenu-search">
          <Icon name="search" size={16} />
          <input
            ref={inputRef}
            type="text"
            data-nmenu-search=""
            placeholder={t("nmenu.search")}
            aria-label={t("nmenu.search")}
            aria-controls={`${NMENU_ID}-grid`}
            aria-activedescendant={highlighted !== undefined ? tileId(highlighted) : undefined}
            autoComplete="off"
            spellCheck={false}
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              setHighlight(-1);
            }}
            onKeyDown={onSearchKeyDown}
          />
          <kbd>{shortcutById("nMenu")?.keys}</kbd>
        </label>
        {visible.length === 0 ? (
          <p className="nd-nmenu-empty" role="status">
            {t("nmenu.noMatch")}
          </p>
        ) : (
          <ul id={`${NMENU_ID}-grid`} className="nd-nmenu-grid" data-nmenu-grid="" aria-label={t("nmenu.apps")} onKeyDown={onGridKeyDown}>
            {visible.map((kind) => {
              const meta = WINDOW_META[kind];
              const state = appItemState(layout, kind, top);
              const subtitle = subtitles[kind];
              const badge = badges[kind];
              const showBadge = badge !== undefined && badge.count > 0;
              const details = [STATE_LABELS[state], subtitle, showBadge ? badge.label : undefined].filter(
                (part): part is string => part !== undefined && part !== "",
              );
              const slot = kinds.indexOf(kind) + 1;
              return (
                <li key={kind}>
                  <button
                    type="button"
                    id={tileId(kind)}
                    className="nd-nmenu-tile"
                    data-kind={kind}
                    data-state={state}
                    data-highlight={kind === highlighted ? "true" : undefined}
                    aria-label={meta.title}
                    aria-describedby={tipId(kind)}
                    aria-keyshortcuts={appSlotAria(slot)}
                    onClick={() => onActivate(kind)}
                  >
                    <span className="nd-nmenu-icon" aria-hidden="true">
                      <Icon name={meta.icon} size={18} />
                    </span>
                    <span className="nd-nmenu-tile-label" aria-hidden="true">
                      {meta.title}
                    </span>
                    {state !== "closed" ? <span className="nd-nmenu-indicator" data-indicator={state} aria-hidden="true" /> : null}
                    {showBadge ? (
                      <span className="nd-badge" data-badge="" aria-hidden="true">
                        {Math.min(badge.count, BADGE_MAX)}
                      </span>
                    ) : null}
                    <span id={tipId(kind)} className="nd-sr-only">
                      {details.join(", ")}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        )}
        {openKinds.length > 0 ? (
          <section className="nd-nmenu-section" aria-labelledby={`${NMENU_ID}-open`}>
            <h3 id={`${NMENU_ID}-open`} className="nd-nmenu-label">
              {t("nmenu.openWindows")} · {openKinds.length}
            </h3>
            <ul className="nd-nmenu-chips" data-nmenu-open="">
              {openKinds.map((kind) => {
                const state = appItemState(layout, kind, top);
                return (
                  <li key={kind}>
                    <button
                      type="button"
                      className="nd-nmenu-chip"
                      data-kind={kind}
                      data-state={state}
                      aria-label={`${WINDOW_META[kind].title}, ${STATE_LABELS[state]}`}
                      onClick={() => onShow(kind)}
                    >
                      <span className="nd-nmenu-indicator" data-indicator={state} aria-hidden="true" />
                      {WINDOW_META[kind].title}
                    </button>
                  </li>
                );
              })}
            </ul>
          </section>
        ) : null}
        <div className="nd-nmenu-foot" data-nmenu-foot="">
          <span>{t("nmenu.footerSlots")}</span>
          <span>{t("nmenu.footerClose")}</span>
        </div>
      </div>
    </div>
  );
}
