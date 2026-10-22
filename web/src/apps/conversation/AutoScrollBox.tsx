/*
 * Área rolável "cola no fim" da Conversa:
 * - começa no fim ao montar com itens;
 * - acompanha o fim quando algo muda e o usuário está no fim (nova mensagem ao vivo, histórico,
 *   cartão, envio próprio via `pinKey`);
 * - rolado para cima, não rola: conta "N novas" pelas mensagens AO VIVO chegadas depois do último
 *   `seq` visto (linhas do histórico não contam);
 * - restauração (spec 093, decisão 2): a posição volta do ui-state do espaço (`selection`), não
 *   mais do storage da sessão; o conteúdo que ainda não alcançou a altura guardada é
 *   reposicionado conforme cresce, até o usuário rolar. `onSave` recebe cada mudança da posição.
 */
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode, type UIEvent as ReactUIEvent } from "react";
import { Icon } from "../../desktop/Icon";
import { t, tp } from "../../i18n";
import { useLanguage } from "../../i18n/react";

const BOTTOM_THRESHOLD_PX = 80;

export interface ScrollItem {
  readonly id: number;
  readonly seq?: number;
}

/** Posição da rolagem: no fim ou num `scrollTop`. */
export type ScrollPosition = { atBottom: true } | { atBottom: false; top: number };

interface ScrollRestore {
  /** Posição salva (lida só na montagem); `null` = sem posição salva (começa no fim). */
  initial: ScrollPosition | null;
  onSave: (position: ScrollPosition) => void;
}

interface Props {
  items: readonly ScrollItem[];
  pinKey?: number;
  tailKey?: unknown;
  label: string;
  className?: string;
  scrollAttrs?: Readonly<Record<string, string>>;
  restore?: ScrollRestore;
  children: ReactNode;
}

function maxSeqOf(items: readonly ScrollItem[]): number {
  let max = 0;
  for (const item of items) if (item.seq !== undefined && item.seq > max) max = item.seq;
  return max;
}

/** Itens ao vivo chegados depois de `seen` (linhas sem seq nunca contam). */
export function countNewer(items: readonly ScrollItem[], seen: number): number {
  let newer = 0;
  for (const item of items) if (item.seq !== undefined && item.seq > seen) newer++;
  return newer;
}

/** Rótulo do botão de ir ao fim. */
function jumpLabel(unseen: number): string {
  if (unseen <= 0) return t("conversation.jumpToEnd");
  return tp("conversation.unseen", unseen);
}

export function AutoScrollBox({ items, pinKey = 0, tailKey, label, className, scrollAttrs, restore, children }: Props) {
  // O rótulo do botão segue o idioma ativo.
  useLanguage();
  const boxRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const [saved] = useState(() => restore?.initial ?? null);
  const saveRef = useRef(restore?.onSave);
  saveRef.current = restore?.onSave;
  const restoreTopRef = useRef<number | null>(saved !== null && !saved.atBottom ? saved.top : null);
  const atBottomRef = useRef(restoreTopRef.current === null);
  const ownTopRef = useRef<number | null>(null);
  const toEnd = (box: HTMLElement): void => {
    box.scrollTop = box.scrollHeight;
    ownTopRef.current = box.scrollTop;
  };
  const toRestored = (box: HTMLElement): void => {
    const top = restoreTopRef.current;
    if (top === null) return;
    const max = Math.max(0, box.scrollHeight - box.clientHeight);
    box.scrollTop = Math.min(top, max);
    ownTopRef.current = box.scrollTop;
    if (max >= top) restoreTopRef.current = null;
  };
  const seenSeqRef = useRef(0);
  const firstIdRef = useRef<number | null>(null);
  const tailRef = useRef(tailKey);
  const pinRef = useRef(pinKey);
  const [atBottom, setAtBottom] = useState(restoreTopRef.current === null);
  const [unseen, setUnseen] = useState(0);
  const maxSeq = maxSeqOf(items);
  const firstId = items[0]?.id ?? null;

  useLayoutEffect(() => {
    const box = boxRef.current;
    const prevSeq = seenSeqRef.current;
    const prevFirst = firstIdRef.current;
    seenSeqRef.current = maxSeq;
    firstIdRef.current = firstId;
    const pinned = pinRef.current !== pinKey;
    pinRef.current = pinKey;
    const tailChanged = tailRef.current !== tailKey;
    tailRef.current = tailKey;
    if (box === null) return;
    if (pinned) {
      restoreTopRef.current = null;
      saveRef.current?.({ atBottom: true });
      atBottomRef.current = true;
      setAtBottom(true);
      setUnseen(0);
    }
    if (restoreTopRef.current !== null) {
      toRestored(box);
      return;
    }
    const changed = maxSeq !== prevSeq || firstId !== prevFirst || pinned || tailChanged;
    if (!changed) return;
    if (atBottomRef.current) {
      toEnd(box);
      return;
    }
    const added = countNewer(items, prevSeq);
    if (added > 0) setUnseen((total) => total + added);
  }, [maxSeq, firstId, pinKey, tailKey, items.length]);

  useEffect(() => {
    const box = boxRef.current;
    const content = contentRef.current;
    if (box === null || content === null || typeof ResizeObserver === "undefined") return;
    const follow = (): void => {
      if (restoreTopRef.current !== null) toRestored(box);
      else if (atBottomRef.current) toEnd(box);
    };
    const observer = new ResizeObserver(follow);
    observer.observe(content);
    observer.observe(box);
    return () => observer.disconnect();
  }, []);

  function onScroll(event: ReactUIEvent<HTMLDivElement>): void {
    const box = event.currentTarget;
    if (ownTopRef.current !== null && box.scrollTop === ownTopRef.current) return;
    ownTopRef.current = null;
    restoreTopRef.current = null;
    const near = box.scrollHeight - box.scrollTop - box.clientHeight < BOTTOM_THRESHOLD_PX;
    atBottomRef.current = near;
    setAtBottom(near);
    if (near) setUnseen(0);
    saveRef.current?.(near ? { atBottom: true } : { atBottom: false, top: box.scrollTop });
  }

  function jumpToEnd(): void {
    const box = boxRef.current;
    if (box !== null) toEnd(box);
    restoreTopRef.current = null;
    atBottomRef.current = true;
    setAtBottom(true);
    setUnseen(0);
    saveRef.current?.({ atBottom: true });
  }

  return (
    <div className="nd-scroll-wrap">
      <div ref={boxRef} onScroll={onScroll} tabIndex={0} role="region" aria-label={label} className={className} {...scrollAttrs}>
        <div ref={contentRef}>{children}</div>
      </div>
      {!atBottom ? (
        // Botão redondo só com o ícone (094/T08, A-11): o rótulo vai no nome acessível e na dica;
        // as mensagens novas aparecem num selo com o número.
        <button type="button" onClick={jumpToEnd} className="nd-btn nd-iconbtn nd-jump" data-unseen={unseen} aria-label={jumpLabel(unseen)} title={jumpLabel(unseen)}>
          <Icon name="arrow-down" size={16} />
          {unseen > 0 ? (
            <span aria-hidden="true" className="nd-jump-count" data-jump-count="">
              {unseen > 99 ? "99" : unseen}
            </span>
          ) : null}
        </button>
      ) : null}
    </div>
  );
}
