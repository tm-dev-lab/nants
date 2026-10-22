/*
 * Lançador (Ctrl+K; spec 093, decisão 11): campo de busca com a lista de janelas, projetos e
 * ações. Setas movem o destaque, Enter executa, Esc fecha (antes de qualquer outro atalho) e
 * devolve o foco a quem abriu. Cada item mostra o atalho dele, quando há.
 */
import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { useT } from "../i18n/react";
import { Icon } from "./Icon";
import { useDesktopLayer } from "./layer";
import { filterLauncher, LAUNCHER_GROUP_LABELS, moveActive, type LauncherGroup, type LauncherItem } from "./launcher";

interface LauncherProps {
  /** Itens fixos; `query` monta os dinâmicos (como "Pedir ao Arquiteto"). */
  items: (query: string) => readonly LauncherItem[];
  onClose: () => void;
}

export function Launcher({ items, onClose }: LauncherProps) {
  const { t } = useT();
  const layer = useDesktopLayer();
  const inputId = useId();
  const listId = useId();
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  const visible = useMemo(() => filterLauncher(items(query), query), [items, query]);
  const current = visible.length === 0 ? -1 : Math.min(Math.max(active, 0), visible.length - 1);
  const optionId = (index: number): string => `${listId}-option-${index}`;

  useEffect(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    inputRef.current?.focus({ preventScroll: true });
    return () => {
      if (opener?.isConnected === true && !(document.activeElement instanceof HTMLElement && document.activeElement !== document.body)) {
        opener.focus({ preventScroll: true });
      }
    };
  }, []);

  useEffect(() => {
    if (current < 0) return;
    document.getElementById(optionId(current))?.scrollIntoView?.({ block: "nearest" });
  }, [current]);

  const run = (item: LauncherItem | undefined): void => {
    if (item === undefined || item.disabled !== undefined) return;
    closeRef.current();
    item.run();
  };

  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>): void => {
    switch (event.key) {
      case "Escape":
        event.preventDefault();
        event.stopPropagation();
        closeRef.current();
        return;
      case "ArrowDown":
      case "ArrowUp":
        event.preventDefault();
        setActive(moveActive(current, event.key === "ArrowDown" ? 1 : -1, visible.length));
        return;
      case "Enter":
        event.preventDefault();
        run(visible[current]);
        return;
      case "Tab":
        // O foco fica no campo: a lista é navegada pelas setas.
        event.preventDefault();
        return;
      default:
    }
  };

  // Itens agrupados para os rótulos, mantendo o índice global.
  const groups: { group: LauncherGroup; entries: { item: LauncherItem; index: number }[] }[] = [];
  visible.forEach((item, index) => {
    const last = groups[groups.length - 1];
    if (last !== undefined && last.group === item.group) last.entries.push({ item, index });
    else groups.push({ group: item.group, entries: [{ item, index }] });
  });

  const node = (
    <div className="nd-launcher-backdrop" data-launcher-backdrop="" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div role="dialog" aria-modal="true" aria-label={t("launcher.label")} className="nd-launcher nd-overlay" data-launcher="" onKeyDown={onKeyDown}>
        <div className="nd-launcher-search">
          <Icon name="search" size={18} />
          <label htmlFor={inputId} className="nd-sr-only">
            {t("launcher.searchLabel")}
          </label>
          <input
            id={inputId}
            ref={inputRef}
            type="text"
            role="combobox"
            aria-expanded={visible.length > 0}
            aria-controls={listId}
            aria-autocomplete="list"
            aria-activedescendant={current >= 0 ? optionId(current) : undefined}
            placeholder={t("launcher.placeholder")}
            autoComplete="off"
            spellCheck={false}
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              setActive(0);
            }}
          />
        </div>
        <div id={listId} role="listbox" aria-label={t("launcher.results")} className="nd-launcher-list">
          {groups.map(({ group, entries }, groupIndex) => (
            <div key={`${group}-${groupIndex}`} role="group" aria-labelledby={`${listId}-group-${groupIndex}`}>
              <div id={`${listId}-group-${groupIndex}`} role="presentation" className="nd-menu-label">
                {LAUNCHER_GROUP_LABELS[group]}
              </div>
              {entries.map(({ item, index }) => (
                <div
                  key={item.id}
                  id={optionId(index)}
                  role="option"
                  aria-selected={index === current}
                  aria-disabled={item.disabled !== undefined ? "true" : undefined}
                  aria-keyshortcuts={item.aria}
                  className="nd-launcher-item"
                  data-launcher-item={item.id}
                  data-active={index === current ? "true" : undefined}
                  onMouseDown={(event) => event.preventDefault()}
                  onMouseMove={() => setActive(index)}
                  onClick={() => run(item)}
                >
                  <Icon name={item.icon} size={16} />
                  <span className="nd-menu-text">{item.label}</span>
                  {item.disabled !== undefined ? (
                    <span className="nd-menu-hint" data-launcher-disabled="">
                      {item.disabled}
                    </span>
                  ) : item.hint !== undefined ? (
                    <span className="nd-menu-hint">{item.hint}</span>
                  ) : null}
                  {item.keys !== undefined ? <kbd>{item.keys}</kbd> : null}
                </div>
              ))}
            </div>
          ))}
          {visible.length === 0 ? (
            <p className="nd-launcher-empty" role="status">
              {t("launcher.nothingFound")}
            </p>
          ) : null}
        </div>
      </div>
    </div>
  );
  return layer === null ? node : createPortal(node, layer);
}
