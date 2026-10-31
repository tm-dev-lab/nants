/*
 * Janela do Nanty (spec 138 T04 e T10, `reference/Main.dc.html`): janela de sistema, disponível em
 * qualquer espaço, inclusive sem projeto. A moldura é a de toda janela (`Window`); as abas Feed,
 * Regras e Ajustes ficam no slot da barra de título, como em Configurações (no celular, a linha
 * logo abaixo dela). O cabeçalho é o da moldura: o avatar no estado atual e o subtítulo vivo (o
 * estado, os projetos observados, o gasto do dia e a saúde do Telegram, `nantySummary`).
 *
 * Aba Feed (T10): o feed (`NantyFeedView`) e a conversa fixa à direita (`NantyChat`). Em janela
 * média ou estreita (e no celular), um seletor alterna entre os dois, cada um com a altura toda.
 *
 * Regras (`NantyRules`) e Ajustes (`NantySettingsView`), T11. O "Mudar horário" do Previsto abre
 * os Ajustes com o campo do resumo da manhã em foco; o "Abrir o Nanty" de Configurações > Agentes
 * abre a janela já nos Ajustes (`requestNantyTab`).
 */
import "./nanty.css";
import "../shared/apps.css";
import { memo, useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type ReactNode } from "react";
import { useT } from "../../i18n/react";
import { useShell } from "../../desktop/shellContext";
import { WindowTools } from "../../desktop/Window";
import type { AppContentProps } from "../registry";
import { NantyAvatar } from "./NantyAvatar";
import type { NantyCard } from "./nantyApi";
import { NantyChat } from "./NantyChat";
import { NantyFeedView } from "./NantyFeed";
import { linkWindow, NANTY_MOOD_LABELS, NANTY_TAB_LABELS, NANTY_TABS, nantyMood, nantyTabByKey, type NantyTab } from "./nantyModel";
import { NantyRules } from "./NantyRules";
import { NantySettingsView } from "./NantySettings";
import { consumeNantyTabRequest, useNanty, useNantyConversation, useNantyTabRequest } from "./nantyStore";
import { useNantyDecisions } from "./useNantyDecisions";

const AVATAR_SIZE = 72;

type Pane = "feed" | "chat";

function FeedEmpty({ pending, avatar }: { pending: number; avatar: ReactNode }) {
  const { t, tp } = useT();
  return (
    <div className="nd-nanty-empty" data-nanty-empty="feed">
      {avatar}
      <h3 className="nd-nanty-empty-title">{pending > 0 ? tp("nanty.feed.pending", pending) : t("nanty.feed.calm")}</h3>
      <p className="nd-nanty-empty-text">{t("nanty.feed.text")}</p>
    </div>
  );
}

/** A aba Feed: o feed e a conversa, lado a lado na janela larga; um de cada vez nas outras. */
function FeedTab({ split, onSettings }: { split: boolean; onSettings: () => void }) {
  const { t } = useT();
  const shell = useShell();
  const { status, feed } = useNanty();
  const messages = useNantyConversation();
  const [pane, setPane] = useState<Pane>("feed");
  const open = useCallback((card: NantyCard) => {
    if (card.project_id !== null) shell.goTo(card.project_id, linkWindow(card));
  }, [shell]);
  const decisions = useNantyDecisions(open);
  const mood = nantyMood(status);
  const avatar = <NantyAvatar mood={mood} size={AVATAR_SIZE} label={t("nanty.avatar.label", { mood: NANTY_MOOD_LABELS[mood] })} />;
  const showFeed = split || pane === "feed";
  const showChat = split || pane === "chat";
  return (
    <div className="nd-nanty-split" data-split={split ? "true" : "false"}>
      {!split ? (
        <div role="group" aria-label={t("nanty.view.label")} className="nd-segment nd-chrome nd-nanty-switch" data-nanty-switch="">
          <button type="button" className="nd-segment-item" aria-pressed={pane === "feed"} onClick={() => setPane("feed")}>
            <span>{t("nanty.tab.feed")}</span>
          </button>
          <button type="button" className="nd-segment-item" aria-pressed={pane === "chat"} onClick={() => setPane("chat")}>
            <span>{t("nanty.convo.title")}</span>
          </button>
        </div>
      ) : null}
      {showFeed ? (
        <div className="nd-nanty-feed-pane" aria-label={t("nanty.feed.label")} role="region">
          <NantyFeedView
            status={status}
            feed={feed}
            decisions={decisions}
            onChangeDigestTime={onSettings}
            empty={<FeedEmpty pending={feed?.waiting.length ?? 0} avatar={avatar} />}
          />
        </div>
      ) : null}
      {showChat ? <NantyChat messages={messages} cards={feed?.waiting ?? []} decisions={decisions} readOnly={status?.instance === "other_instance"} /> : null}
    </div>
  );
}

function NantyContentView({ size }: AppContentProps) {
  const { t } = useT();
  const [tab, setTab] = useState<NantyTab>("feed");
  const [focusDigest, setFocusDigest] = useState(false);
  const baseId = useId();
  const tabRefs = useRef(new Map<NantyTab, HTMLButtonElement>());
  const request = useNantyTabRequest();
  useEffect(() => {
    if (request === null) return;
    setTab(request.tab);
    consumeNantyTabRequest(request.id);
  }, [request]);
  const tabId = (k: NantyTab): string => `${baseId}-tab-${k}`;
  const panelId = `${baseId}-panel`;
  // Cada aba abre no topo (a rolagem de uma não passa para a outra). O "Mudar horário" é a exceção:
  // os Ajustes levam ao campo do resumo da manhã (`NantySettingsView`).
  const panelRef = useRef<HTMLElement | null>(null);
  const focusDigestRef = useRef(focusDigest);
  focusDigestRef.current = focusDigest;
  useLayoutEffect(() => {
    if (focusDigestRef.current) return;
    const panel = panelRef.current;
    if (panel !== null) panel.scrollTop = 0;
  }, [tab]);

  const onTabKeyDown = (event: ReactKeyboardEvent<HTMLElement>): void => {
    const next = nantyTabByKey(tab, event.key);
    if (next === null) return;
    event.preventDefault();
    setTab(next);
    tabRefs.current.get(next)?.focus();
  };

  return (
    <div className="nd-nanty-app" data-nanty-app="" data-size={size} data-tab={tab}>
      <WindowTools inlineOutsideWindow>
        <div role="tablist" aria-label={t("nanty.tabs")} aria-orientation="horizontal" className="nd-nanty-tabs" onKeyDown={onTabKeyDown}>
          {NANTY_TABS.map((k) => {
            const selected = k === tab;
            return (
              <button
                key={k}
                ref={(el) => {
                  if (el === null) tabRefs.current.delete(k);
                  else tabRefs.current.set(k, el);
                }}
                type="button"
                role="tab"
                id={tabId(k)}
                aria-selected={selected}
                aria-controls={panelId}
                tabIndex={selected ? 0 : -1}
                className="nd-nanty-tab"
                data-nanty-tab={k}
                onClick={() => setTab(k)}
              >
                {NANTY_TAB_LABELS[k]}
              </button>
            );
          })}
        </div>
      </WindowTools>
      <section ref={panelRef} role="tabpanel" id={panelId} aria-labelledby={tabId(tab)} className="nd-nanty-panel nd-app-scroll" tabIndex={-1}>
        <h2 className="nd-sr-only">{NANTY_TAB_LABELS[tab]}</h2>
        {tab === "feed" ? (
          <FeedTab
            split={size === "wide"}
            onSettings={() => {
              setFocusDigest(true);
              setTab("settings");
            }}
          />
        ) : tab === "rules" ? (
          <NantyRules />
        ) : (
          <NantySettingsView focusDigest={focusDigest} onFocused={() => setFocusDigest(false)} />
        )}
      </section>
    </div>
  );
}

export const NantyContent = memo(NantyContentView);
