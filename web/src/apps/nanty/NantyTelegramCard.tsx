/*
 * Telegram nos Ajustes do Nanty (spec 138 T11, `reference/Settings.dc.html`): o estado do canal, o
 * token, o pareamento, a mensagem de teste, desparear, o passo a passo do BotFather, a foto do bot
 * e o aviso de privacidade.
 *
 * - **Token:** o campo nasce vazio e nunca mostra o token salvo, só a máscara que o servidor devolve
 *   (no texto de apoio). Depois de salvar, o campo é limpo: o token só vive no campo enquanto a
 *   pessoa digita.
 * - **Pareamento (C-11):** o código e o link `t.me/<bot>?start=<código>` só aparecem aqui, com a
 *   validade e um código novo. O servidor troca o código vencido no `GET`; a tela relê quando a vez
 *   do código passa e quando o status do Nanty muda (o pareamento feito no Telegram).
 * - **Estado:** conectado, falta parear, token inválido, conflito ("um bot por instalação"), rede,
 *   limite e sem bot, pelo dicionário.
 */
import { useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from "react";
import type { ApiResult } from "../../api/client";
import { Icon } from "../../desktop/Icon";
import type { MessageKey } from "../../i18n";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import { getNantyTelegram, postNantyTelegram, postNantyToken, type NantyHealth, type NantyTelegram } from "./nantyApi";
import { minutesText } from "./nantySettingsModel";
import { useNanty } from "./nantyStore";

/** Foto do bot (a pose "atento", 512x512), servida pelo próprio Nants. */
export const NANTY_BOT_PHOTO = "/brand/nanty-bot.png";

type TgState = NantyHealth | "unpaired";

const STATE_KEYS: Readonly<Record<TgState, MessageKey>> = {
  off: "nanty.tg.state.off",
  not_configured: "nanty.tg.state.not_configured",
  connecting: "nanty.tg.state.connecting",
  connected: "nanty.tg.state.connected",
  unpaired: "nanty.tg.state.unpaired",
  invalid_token: "nanty.tg.state.invalid_token",
  conflict: "nanty.tg.state.conflict",
  network_error: "nanty.tg.state.network_error",
  rate_limited: "nanty.tg.state.rate_limited",
};

const DETAIL_KEYS: Readonly<Record<Exclude<TgState, "connected" | "unpaired">, MessageKey>> = {
  off: "nanty.tg.detail.off",
  not_configured: "nanty.tg.detail.not_configured",
  connecting: "nanty.tg.detail.connecting",
  invalid_token: "nanty.tg.detail.invalid_token",
  conflict: "nanty.tg.detail.conflict",
  network_error: "nanty.tg.detail.network_error",
  rate_limited: "nanty.tg.detail.rate_limited",
};

const TONES: Readonly<Record<TgState, string>> = {
  off: "neutral",
  not_configured: "neutral",
  connecting: "amber",
  connected: "green",
  unpaired: "amber",
  invalid_token: "rose",
  conflict: "rose",
  network_error: "amber",
  rate_limited: "amber",
};

/** O estado mostrado: sem token é "sem bot"; conectado sem dono, "falta parear". */
function telegramState(tg: NantyTelegram): TgState {
  if (!tg.configured) return "not_configured";
  if (tg.health === "connected" && !tg.paired) return "unpaired";
  return tg.health;
}

type Notice = { tone: "ok" | "warn"; text: string };

interface Props {
  /** A espera com a aba aberta (gravada com os outros ajustes). */
  hold: ReactNode;
}

export function NantyTelegramCard({ hold }: Props) {
  const { t } = useT();
  const { status } = useNanty();
  const headingId = useId();
  const tokenId = useId();
  const tokenHelpId = useId();
  const [tg, setTg] = useState<NantyTelegram | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<Notice | null>(null);
  // O relógio da validade: o instante da última leitura e o "agora" de cada segundo.
  const [readAt, setReadAt] = useState(() => Date.now());
  const [now, setNow] = useState(() => Date.now());
  const alive = useRef(true);

  const apply = (next: NantyTelegram): void => {
    setTg(next);
    const at = Date.now();
    setReadAt(at);
    setNow(at);
  };

  const reload = (): void => {
    void getNantyTelegram().then((r) => {
      if (!alive.current) return;
      if (r.ok) {
        apply(r.data);
        setLoadError(null);
      } else setLoadError(t("nanty.tg.loadFailed", { error: apiFailureText(r.error) }));
    });
  };

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  // O pareamento feito no Telegram e a saúde do canal mudam o status do Nanty: relê o canal.
  useEffect(reload, [status?.telegram, status?.telegram_paired, status?.telegram_configured]);

  // Com o código na tela: um tique por segundo e a releitura quando a vez do código passa.
  const pairing = tg?.pairing ?? null;
  useEffect(() => {
    if (pairing?.state !== "open" || pairing.seconds_to_next === null) return undefined;
    const tick = setInterval(() => setNow(Date.now()), 1000);
    const next = setTimeout(reload, (pairing.seconds_to_next + 1) * 1000);
    return () => {
      clearInterval(tick);
      clearTimeout(next);
    };
  }, [pairing?.state, pairing?.code, pairing?.seconds_to_next]);

  const run = async (call: () => Promise<ApiResult<NantyTelegram>>, done: MessageKey | null): Promise<boolean> => {
    if (busy) return false;
    setBusy(true);
    setNotice(null);
    const result = await call();
    if (!alive.current) return false;
    setBusy(false);
    if (!result.ok) {
      setNotice({ tone: "warn", text: t("nanty.tg.failed", { error: apiFailureText(result.error) }) });
      return false;
    }
    apply(result.data);
    if (done !== null) setNotice({ tone: "ok", text: t(done) });
    return true;
  };

  const saveToken = async (event: FormEvent<HTMLFormElement>): Promise<void> => {
    event.preventDefault();
    const value = token.trim();
    if (value === "") return;
    // O campo é limpo já no envio: o token não fica no estado da tela depois de salvo.
    setToken("");
    await run(() => postNantyToken(value), "nanty.tg.tokenStored");
  };

  if (tg === null) {
    return (
      <section className="nd-nanty-pref" data-nanty-pref="telegram" aria-labelledby={headingId}>
        <h3 id={headingId} className="nd-nanty-pref-title">
          Telegram
        </h3>
        {loadError !== null ? (
          <p role="alert" className="nd-field-error">
            {loadError}
          </p>
        ) : null}
        {hold}
      </section>
    );
  }

  const state = telegramState(tg);
  const elapsed = Math.floor((now - readAt) / 1000);
  const open = tg.pairing.state === "open" && tg.pairing.code !== null;
  const detail =
    state === "connected"
      ? tg.bot_username !== null
        ? t("nanty.tg.detail.paired", { bot: tg.bot_username })
        : t("nanty.tg.detail.pairedNoName")
      : state === "unpaired"
        ? t("nanty.tg.detail.unpaired", { bot: tg.bot_username ?? "" })
        : t(DETAIL_KEYS[state]);
  const canPair = tg.configured && !tg.paired && tg.bot_username !== null;

  return (
    <section className="nd-nanty-pref" data-nanty-pref="telegram" aria-labelledby={headingId}>
      <div className="nd-nanty-pref-head">
        <h3 id={headingId} className="nd-nanty-pref-title">
          Telegram
        </h3>
        <span className="nd-nanty-state" data-nanty-tg-state={state} aria-label={t("nanty.tg.label", { state: t(STATE_KEYS[state]) })}>
          <span className="nd-dot" data-tone={TONES[state]} aria-hidden="true" />
          <span aria-hidden="true">{t(STATE_KEYS[state])}</span>
        </span>
      </div>
      <p className="nd-nanty-pref-text" data-nanty-tg-detail="">
        {detail}
      </p>
      {notice !== null ? (
        <p role="status" className="nd-nanty-notice" data-tone={notice.tone} data-nanty-tg-notice="">
          {notice.text}
        </p>
      ) : null}

      <form className="nd-field" onSubmit={(event) => void saveToken(event)} data-nanty-token-form="">
        <label htmlFor={tokenId} className="nd-field-label">
          {t("nanty.tg.token")}
        </label>
        <div className="nd-nanty-inline">
          <input
            id={tokenId}
            type="password"
            className="nd-input"
            value={token}
            onChange={(event) => setToken(event.target.value)}
            placeholder={tg.configured ? t("nanty.tg.tokenSaved", { masked: tg.bot_token_masked }) : t("nanty.tg.tokenEmpty")}
            aria-describedby={tokenHelpId}
            autoComplete="off"
            spellCheck={false}
            data-nanty-token=""
          />
          <button type="submit" className="nd-btn" disabled={busy || token.trim() === ""}>
            <Icon name="key" size={14} />
            {t("nanty.tg.tokenSave")}
          </button>
        </div>
        <p id={tokenHelpId} className="nd-field-hint">
          {tg.configured ? `${t("nanty.tg.tokenSaved", { masked: tg.bot_token_masked })} ` : ""}
          {t("nanty.tg.tokenHint")}
        </p>
      </form>

      {open && tg.pairing.code !== null ? (
        <div className="nd-nanty-pairing" data-nanty-pairing="">
          <span className="nd-field-label">{t("nanty.tg.code")}</span>
          <strong className="nd-nanty-code" data-nanty-code="">
            {tg.pairing.code}
          </strong>
          {tg.pairing.link !== null ? (
            <a className="nd-nanty-link" href={tg.pairing.link} target="_blank" rel="noopener noreferrer" data-nanty-link="">
              <Icon name="external" size={14} />
              {t("nanty.tg.link")}
            </a>
          ) : null}
          <p className="nd-field-hint">{t("nanty.tg.pairHint")}</p>
          {tg.pairing.expires_in !== null && tg.pairing.seconds_to_next !== null ? (
            <p className="nd-field-hint nd-num" data-nanty-validity="">
              {t("nanty.tg.validity", { time: minutesText(tg.pairing.expires_in - elapsed), next: minutesText(tg.pairing.seconds_to_next - elapsed) })}
            </p>
          ) : null}
        </div>
      ) : null}
      {tg.pairing.state === "locked" ? (
        <p className="nd-field-warn" data-nanty-locked="">
          {t("nanty.tg.locked")}
        </p>
      ) : null}
      {tg.configured && !tg.paired && tg.bot_username === null && state !== "invalid_token" && state !== "conflict" ? (
        <p className="nd-field-hint">{t("nanty.tg.needConnection")}</p>
      ) : null}

      <div className="nd-nanty-actions">
        {canPair ? (
          <button type="button" className="nd-btn" disabled={busy} onClick={() => void run(() => postNantyTelegram("pair"), null)} data-nanty-pair="">
            <Icon name={open ? "refresh" : "link"} size={14} />
            {open || tg.pairing.state === "locked" ? t("nanty.tg.pairAgain") : t("nanty.tg.pair")}
          </button>
        ) : null}
        {tg.paired ? (
          <>
            <button type="button" className="nd-btn" disabled={busy} onClick={() => void run(() => postNantyTelegram("test"), "nanty.tg.testSent")} data-nanty-test="">
              <Icon name="send" size={14} />
              {t("nanty.tg.test")}
            </button>
            <button type="button" className="nd-btn" data-variant="ghost" disabled={busy} onClick={() => void run(() => postNantyTelegram("unpair"), "nanty.tg.unpaired")} data-nanty-unpair="">
              {t("nanty.tg.unpair")}
            </button>
          </>
        ) : null}
        {tg.configured ? (
          <button type="button" className="nd-btn" data-variant="ghost" disabled={busy} onClick={() => void run(() => postNantyToken(""), "nanty.tg.tokenRemoved")} data-nanty-token-remove="">
            <Icon name="trash" size={14} />
            {t("nanty.tg.tokenRemove")}
          </button>
        ) : null}
      </div>

      <p className="nd-nanty-pref-note" data-nanty-privacy="">
        {t("nanty.tg.privacy")}
      </p>
      <p className="nd-nanty-pref-note" data-nanty-sleep="">
        {t("nanty.tg.sleep")}
      </p>

      {hold}

      {!tg.paired ? (
        <div className="nd-nanty-steps" data-nanty-steps="">
          <h4 className="nd-nanty-steps-title">{t("nanty.tg.steps.title")}</h4>
          <ol className="nd-nanty-steps-list">
            <li>{t("nanty.tg.steps.1")}</li>
            <li>{t("nanty.tg.steps.2")}</li>
            <li>{t("nanty.tg.steps.3")}</li>
            <li>{t("nanty.tg.steps.4")}</li>
          </ol>
          <p className="nd-field-hint">{t("nanty.tg.steps.note")}</p>
        </div>
      ) : null}

      <div className="nd-nanty-photo" data-nanty-photo="">
        <img src={NANTY_BOT_PHOTO} alt="" width={48} height={48} className="nd-nanty-photo-img" />
        <span className="nd-nanty-photo-text">
          <span className="nd-nanty-photo-title">{t("nanty.photo.title")}</span>
          <span className="nd-nanty-empty-note">{t("nanty.photo.help")}</span>
          <a className="nd-nanty-photo-link" href={NANTY_BOT_PHOTO} download="nanty-bot.png">
            {t("nanty.photo.download")}
          </a>
        </span>
      </div>
    </section>
  );
}
