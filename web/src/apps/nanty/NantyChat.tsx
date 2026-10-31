/*
 * Conversa com o Nanty (spec 138 T10, `reference/Main.dc.html`): a mesma do Telegram, com a origem
 * de cada mensagem (você ou o Nanty, pela janela ou pelo Telegram).
 *
 * - **Texto do Nanty:** o texto fixo pelo dicionário (`code` + `params`, com as linhas extras) e o
 *   texto do modelo como veio; os cartões que a mensagem cita levam os mesmos botões do feed.
 * - **Voz (T09):** a nota de voz toca pela rota `GET /api/nanty/audio/:id`, com a transcrição no
 *   texto. A rota aceita `Range` (o Safari, inclusive o do iPhone, só toca assim). Navegador que
 *   não toca Ogg/Opus (`canPlayType`): a transcrição fica, com um aviso discreto e o baixar.
 * - **Envio:** `POST /api/nanty/conversation`; a resposta chega pelo `nanty_changed` e fica só na
 *   janela (T08). O gravar aparece desligado: a gravação pela janela fica para depois.
 */
import { useEffect, useId, useLayoutEffect, useRef, useState, type FormEvent } from "react";
import { formatTime, type MessageKey } from "../../i18n";
import { apiFailureText } from "../../i18n/serverCodes";
import { useT } from "../../i18n/react";
import { Icon } from "../../desktop/Icon";
import { NantyAvatar } from "./NantyAvatar";
import { nantyAudioUrl, sendNantyMessage, type NantyAudio, type NantyCard, type NantyMessage } from "./nantyApi";
import { CardActions } from "./NantyFeed";
import { nantyLinesText } from "./nantyText";
import { reloadNantyConversation } from "./nantyStore";
import type { NantyDecisions } from "./useNantyDecisions";

/** Maior mensagem aceita pelo servidor (o limite do Telegram). */
const MAX_CHARS = 4096;
/** Distância do fim que ainda conta como "no fim" (acompanha as mensagens novas). */
const BOTTOM_PX = 80;

/** Os comandos fixos (T08), na ordem da ajuda: o nome no idioma e a descrição do servidor. */
const COMMANDS: readonly { name: MessageKey; about: MessageKey }[] = [
  { name: "nanty.convo.cmd.status", about: "nanty.command.status" },
  { name: "nanty.convo.cmd.summary", about: "nanty.command.summary" },
  { name: "nanty.convo.cmd.pause", about: "nanty.command.pause" },
  { name: "nanty.convo.cmd.silence", about: "nanty.command.silence" },
  { name: "nanty.convo.cmd.help", about: "nanty.command.help" },
];

/** O navegador toca Opus em OGG? (o Safari antigo não toca; os demais sim). */
function canPlayOgg(): boolean {
  if (typeof document === "undefined") return false;
  const probe = document.createElement("audio");
  return typeof probe.canPlayType === "function" && probe.canPlayType('audio/ogg; codecs="opus"') !== "";
}

function clock(secs: number): string {
  const s = Math.max(0, Math.round(secs));
  return `${String(Math.floor(s / 60))}:${String(s % 60).padStart(2, "0")}`;
}

/** A nota de voz: tocar e pausar, o progresso e a duração; sem suporte, o aviso e o baixar. */
function VoiceNote({ audio }: { audio: NantyAudio }) {
  const { t } = useT();
  const ref = useRef<HTMLAudioElement | null>(null);
  const [playable] = useState(canPlayOgg);
  const [playing, setPlaying] = useState(false);
  const [failed, setFailed] = useState(false);
  const [at, setAt] = useState(0);
  const url = nantyAudioUrl(audio);
  const total = audio.duration_secs;
  const download = (
    <a className="nd-btn nd-btn-sm" data-variant="ghost" href={url} download={`nanty-${audio.id}.ogg`} data-nanty-download="">
      <Icon name="arrow-down" size={14} />
      {t("nanty.convo.download")}
    </a>
  );
  if (!playable || failed) {
    return (
      <div className="nd-nanty-voice" data-nanty-voice="unsupported">
        <p className="nd-nanty-voice-note">{t(failed ? "nanty.convo.playFailed" : "nanty.convo.cantPlay")}</p>
        {download}
      </div>
    );
  }
  const toggle = (): void => {
    const el = ref.current;
    if (el === null) return;
    if (playing) {
      el.pause();
      return;
    }
    const started = el.play();
    if (started !== undefined) void started.catch(() => setFailed(true));
  };
  const pct = total > 0 ? Math.min(100, (at / total) * 100) : 0;
  return (
    <div className="nd-nanty-voice" data-nanty-voice="player">
      <audio
        ref={ref}
        src={url}
        preload="none"
        onPlay={() => setPlaying(true)}
        onPause={() => setPlaying(false)}
        onEnded={() => {
          setPlaying(false);
          setAt(0);
        }}
        onTimeUpdate={(e) => setAt(e.currentTarget.currentTime)}
        onError={() => setFailed(true)}
      />
      <button type="button" className="nd-btn nd-iconbtn" data-variant="ghost" aria-label={t(playing ? "nanty.convo.pause" : "nanty.convo.play")} onClick={toggle}>
        <Icon name={playing ? "pause" : "play"} size={16} />
      </button>
      <span className="nd-nanty-voice-track" aria-hidden="true">
        <span className="nd-nanty-voice-fill" style={{ width: `${String(pct)}%` }} />
      </span>
      <span className="nd-nanty-voice-time nd-num">{clock(playing || at > 0 ? at : total)}</span>
    </div>
  );
}

function originKey(m: NantyMessage): MessageKey {
  if (m.from === "person") return m.channel === "telegram" ? "nanty.convo.from.person.telegram" : "nanty.convo.from.person.web";
  return m.channel === "telegram" ? "nanty.convo.from.nanty.telegram" : "nanty.convo.from.nanty.web";
}

function Meta({ m }: { m: NantyMessage }) {
  const { t } = useT();
  const parts = [t(originKey(m))];
  if (m.from === "person" && m.voice) parts.push(t("nanty.convo.voiceNote"));
  if (m.from === "nanty" && m.audio !== null) parts.push(t("nanty.convo.voiceText"));
  parts.push(formatTime(m.at_ms));
  return (
    <span className="nd-nanty-msg-meta" data-nanty-origin={`${m.from}:${m.channel}`}>
      {parts.join(" · ")}
    </span>
  );
}

function PersonMessage({ m }: { m: NantyMessage }) {
  const { t } = useT();
  const text = m.text !== null && m.text.trim() !== "" ? m.text : m.voice ? t("nanty.convo.noTranscript") : "";
  return (
    <li className="nd-nanty-msg" data-from="person" data-nanty-msg={m.id}>
      {m.audio !== null ? <VoiceNote audio={m.audio} /> : null}
      {text !== "" ? <p className="nd-nanty-bubble">{text}</p> : null}
      <Meta m={m} />
    </li>
  );
}

function NantyMessageView({ m, cards, decisions }: { m: NantyMessage; cards: readonly NantyCard[]; decisions: NantyDecisions }) {
  const paragraphs = [...nantyLinesText(m.code, m.params, m.lines), ...(m.text !== null && m.text.trim() !== "" ? [m.text] : [])];
  const cited = m.card_ids.map((id) => cards.find((c) => c.id === id)).filter((c): c is NantyCard => c !== undefined);
  return (
    <li className="nd-nanty-msg" data-from="nanty" data-nanty-msg={m.id}>
      <NantyAvatar mood="attentive" size={26} still />
      <div className="nd-nanty-msg-body">
        {m.audio !== null ? <VoiceNote audio={m.audio} /> : null}
        {paragraphs.length > 0 ? (
          <div className="nd-nanty-bubble">
            {paragraphs.map((p, i) => (
              <p key={i}>{p}</p>
            ))}
          </div>
        ) : null}
        {cited.map((card) => (
          <CardActions key={card.id} card={card} decisions={decisions} primary={false} origin="chat" />
        ))}
        <Meta m={m} />
      </div>
    </li>
  );
}

function EmptyChat() {
  const { t } = useT();
  return (
    <div className="nd-nanty-chat-empty" data-nanty-chat-empty="">
      <p>{t("nanty.convo.empty")}</p>
      <dl className="nd-nanty-commands">
        {COMMANDS.map((c) => (
          <div key={c.name} className="nd-nanty-command">
            <dt>{t(c.name)}</dt>
            <dd>{t(c.about)}</dd>
          </div>
        ))}
      </dl>
    </div>
  );
}

interface ChatProps {
  messages: readonly NantyMessage[] | null;
  /** Os cartões abertos, para os botões das mensagens que os citam. */
  cards: readonly NantyCard[];
  decisions: NantyDecisions;
  /** O Nanty está ativo noutra instância: a conversa é só leitura aqui. */
  readOnly: boolean;
}

export function NantyChat({ messages, cards, decisions, readOnly }: ChatProps) {
  const { t } = useT();
  const hintId = useId();
  const listRef = useRef<HTMLOListElement | null>(null);
  const atBottom = useRef(true);
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const list = messages ?? [];

  // Acompanha o fim quando chega mensagem e a pessoa está no fim (ou na primeira leitura).
  useLayoutEffect(() => {
    const el = listRef.current;
    if (el !== null && atBottom.current) el.scrollTop = el.scrollHeight;
  }, [list.length]);
  useEffect(() => {
    const el = listRef.current;
    if (el === null) return undefined;
    const onScroll = (): void => {
      atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < BOTTOM_PX;
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, []);

  const submit = async (event: FormEvent): Promise<void> => {
    event.preventDefault();
    const body = text.trim();
    if (body === "" || sending || readOnly) return;
    setSending(true);
    setError(null);
    const r = await sendNantyMessage(body);
    setSending(false);
    if (!r.ok) {
      setError(apiFailureText(r.error));
      return;
    }
    setText("");
    atBottom.current = true;
    reloadNantyConversation();
  };

  const notice = decisions.notice !== null && decisions.notice.origin === "chat" ? decisions.notice : null;
  return (
    <aside className="nd-nanty-chat" aria-label={t("nanty.convo.label")} data-nanty-chat="">
      <div className="nd-nanty-chat-head">
        <h3 className="nd-nanty-chat-title">{t("nanty.convo.title")}</h3>
        <span className="nd-nanty-chat-sub">{t("nanty.convo.same")}</span>
      </div>
      <ol ref={listRef} className="nd-nanty-chat-list" aria-label={t("nanty.convo.label")} aria-live="polite" data-nanty-chat-list="">
        {list.length === 0 ? (
          <li className="nd-nanty-msg" data-from="hint">
            <EmptyChat />
          </li>
        ) : (
          list.map((m) => (m.from === "person" ? <PersonMessage key={m.id} m={m} /> : <NantyMessageView key={m.id} m={m} cards={cards} decisions={decisions} />))
        )}
      </ol>
      {notice !== null ? (
        <p role="status" className="nd-nanty-notice" data-tone={notice.tone} data-nanty-notice="">
          {notice.text}
        </p>
      ) : null}
      {readOnly ? <p className="nd-nanty-chat-error">{t("nanty.outcome.unavailable")}</p> : null}
      {error !== null ? (
        <p role="alert" className="nd-nanty-chat-error" data-nanty-send-error="">
          {error}
        </p>
      ) : null}
      <form className="nd-nanty-composer" onSubmit={(e) => void submit(e)}>
        <input
          className="nd-nanty-input"
          type="text"
          value={text}
          maxLength={MAX_CHARS}
          placeholder={t("nanty.convo.placeholder")}
          aria-label={t("nanty.convo.placeholder")}
          disabled={readOnly}
          onChange={(e) => setText(e.target.value)}
        />
        <button type="submit" className="nd-btn nd-iconbtn" data-variant="ghost" aria-label={t("nanty.convo.send")} disabled={readOnly || sending || text.trim() === ""}>
          <Icon name="send" size={16} />
        </button>
        <span className="nd-nanty-record" title={t("nanty.convo.recordLater")}>
          <button type="button" className="nd-btn nd-iconbtn" aria-label={t("nanty.convo.record")} aria-describedby={hintId} disabled data-nanty-record="">
            <Icon name="mic" size={16} />
          </button>
        </span>
        <span id={hintId} className="nd-sr-only">
          {t("nanty.convo.recordLater")}
        </span>
      </form>
    </aside>
  );
}
