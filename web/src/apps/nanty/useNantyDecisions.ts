/*
 * Decisões da janela do Nanty (spec 138 T10): o clique num botão do cartão (no feed ou na conversa)
 * e o desfazer. A rota responde o que o cartão diz (`nanty.outcome.*`) ou um erro com código
 * (cartão vencido, token trocado); os dois viram uma linha de estado pelo dicionário. Depois de
 * qualquer resposta, o feed se relê (o `nanty_changed` também chega, e o GET em voo não se repete).
 */
import { useCallback, useMemo, useRef, useState } from "react";
import { t } from "../../i18n";
import { apiFailureText } from "../../i18n/serverCodes";
import { decideNantyCard, undoNanty, type NantyCard, type NantyOutcome, type NantyUndo } from "./nantyApi";
import type { CardChoice } from "./nantyModel";
import { nantyCodeText } from "./nantyText";
import { reloadNantyFeed } from "./nantyStore";

export type NoticeOrigin = "feed" | "chat";

interface NantyNotice {
  origin: NoticeOrigin;
  /** `ok`: feito como pedido; `warn`: não foi feito (expirou, já decidido, erro). */
  tone: "ok" | "warn";
  text: string;
}

export interface NantyDecisions {
  /** Ids dos cartões (e dos desfazer) com o pedido em voo: os botões ficam desligados. */
  pending: ReadonlySet<string>;
  notice: NantyNotice | null;
  decide: (card: NantyCard, choice: CardChoice, origin?: NoticeOrigin) => Promise<void>;
  undo: (undo: NantyUndo) => Promise<void>;
}

const OK_OUTCOMES: readonly string[] = ["done", "declined", "snoozed", "undone"];

function outcomeNotice(outcome: NantyOutcome, origin: NoticeOrigin): NantyNotice {
  const text = nantyCodeText(outcome.code, outcome.params, outcome.error_params) ?? t("nanty.result.other");
  return { origin, tone: OK_OUTCOMES.includes(outcome.outcome) ? "ok" : "warn", text };
}

/** `open`: o link "abrir no Nants" abre a janela do projeto, sem passar pela rota. */
export function useNantyDecisions(open: (card: NantyCard) => void): NantyDecisions {
  const [pending, setPending] = useState<ReadonlySet<string>>(new Set());
  const [notice, setNotice] = useState<NantyNotice | null>(null);
  const openRef = useRef(open);
  openRef.current = open;

  const run = useCallback(async (id: string, origin: NoticeOrigin, call: () => ReturnType<typeof undoNanty>): Promise<void> => {
    setPending((p) => new Set([...p, id]));
    try {
      const r = await call();
      setNotice(r.ok ? outcomeNotice(r.data, origin) : { origin, tone: "warn", text: apiFailureText(r.error) });
    } finally {
      setPending((p) => {
        const next = new Set(p);
        next.delete(id);
        return next;
      });
      reloadNantyFeed();
    }
  }, []);

  const decide = useCallback(
    async (card: NantyCard, choice: CardChoice, origin: NoticeOrigin = "feed"): Promise<void> => {
      const token = card.token;
      if (token === undefined) return;
      if (choice === "open") {
        openRef.current(card);
        return;
      }
      await run(card.id, origin, () => decideNantyCard(card.id, token, choice));
    },
    [run],
  );

  const undo = useCallback((u: NantyUndo): Promise<void> => run(u.id, "feed", () => undoNanty(u.id, u.token)), [run]);

  return useMemo(() => ({ pending, notice, decide, undo }), [pending, notice, decide, undo]);
}
