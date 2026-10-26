/*
 * Página de entrada (spec 112, decisões 2 e 3; T02): o que o servidor mostra, em `/connect`, ao
 * aparelho sem dispositivo autorizado com a rede local ligada.
 *
 * - A logo e, logo depois dela, o slogan (regra de identidade); o pedido do código de 6 dígitos,
 *   numérico (`inputmode`), que vai para `POST /api/devices/pair`. Acertou: o servidor grava o
 *   cookie e a página vai para o `?next=` validado (spec 128, a página em que o aparelho estava
 *   quando foi negado), senão para `/`. Errou: a mensagem do caso (código errado com as tentativas que
 *   restam, código invalidado, nenhum código ativo, aguarde um segundo).
 * - `?reason=link_invalid`: o link de entrada do terminal expirou ou já foi usado; a página explica
 *   como obter outro, sem expor nada.
 * - Ao abrir, uma única consulta (`GET /api/devices`) confere se o aparelho já está autorizado
 *   (por exemplo, o cookie voltou depois de um refresh): se estiver, a página vai para o mesmo
 *   destino, sem pedir código. Nenhuma outra chamada à API: sem dispositivo, todas respondem 401.
 */
import { useEffect, useId, useMemo, useRef, useState, type CSSProperties, type FormEvent } from "react";
import { safeNext } from "../api/client";
import { listDevices, pairDevice } from "../api/devices";
import { NantsSymbol } from "../desktop/topbar/BrandMark";
import { BRAND_NAME, BRAND_SLOGAN } from "../desktop/brand";
import { Icon } from "../desktop/Icon";
import { applyThemeMeta, useResolvedTheme } from "../desktop/theme/appearance";
import { themeVariables } from "../desktop/theme/tokens";
import { useT } from "../i18n/react";
import { apiFailureText } from "../i18n/serverCodes";
import { codeDigits, pairErrorKind } from "../apps/settings/devicesModel";
import "./connect.css";

/** Navegação depois do acerto (substituível nos testes). */
export const connectEnvironment = {
  enter: (to: string): void => window.location.replace(to),
  search: (): string => window.location.search,
};

/** Destino depois da entrada: o `?next=` aceito por {@link safeNext}, senão `/`. */
export function entryTarget(search: string): string {
  return safeNext(new URLSearchParams(search).get("next")) ?? "/";
}

/** Motivo vindo do servidor na URL (`?reason=link_invalid`). */
export function entryReason(search: string): "link_invalid" | null {
  return new URLSearchParams(search).get("reason") === "link_invalid" ? "link_invalid" : null;
}

export function ConnectPage() {
  const { t, tp } = useT();
  const [code, setCode] = useState("");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const inputId = useId();
  const errorId = useId();
  const search = connectEnvironment.search();
  const linkInvalid = entryReason(search) === "link_invalid";
  const target = entryTarget(search);
  // Sem a preferência salva (a API pede dispositivo), o tema segue o sistema.
  const theme = useResolvedTheme("auto");
  const variables = useMemo(() => themeVariables(theme) as CSSProperties, [theme]);
  useEffect(() => applyThemeMeta(theme), [theme]);

  // Já autorizado (o cookie voltou): segue para o destino sem pedir código.
  useEffect(() => {
    let alive = true;
    void listDevices().then((result) => {
      if (alive && result.ok) connectEnvironment.enter(target);
    });
    return () => {
      alive = false;
    };
  }, [target]);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (sending) return;
    if (code.length !== 6) {
      setError(t("connect.error.incomplete"));
      inputRef.current?.focus();
      return;
    }
    setSending(true);
    setError(null);
    const result = await pairDevice(code);
    if (result.ok) {
      connectEnvironment.enter(target);
      return;
    }
    setSending(false);
    const kind = pairErrorKind(result.error);
    switch (kind.kind) {
      case "invalid":
        setError(tp("connect.error.invalid", kind.attemptsLeft, { count: kind.attemptsLeft }));
        break;
      case "locked":
        setError(t("connect.error.locked"));
        break;
      case "inactive":
        setError(t("connect.error.inactive"));
        break;
      case "rate_limited":
        setError(t("connect.error.rateLimited"));
        break;
      case "other":
        setError(t("connect.error.failed", { message: apiFailureText(result.error) }));
        break;
    }
    setCode("");
    inputRef.current?.focus();
  };

  return (
    <div className="nd-root nd-connect-root" data-theme={theme} style={variables}>
    <main className="nd-connect" data-connect="">
      <div className="nd-connect-card">
        <div className="nd-connect-brand" data-brand="">
          <NantsSymbol size={36} />
          <span className="nd-connect-name">{BRAND_NAME}</span>
        </div>
        <p className="nd-connect-slogan" data-connect-slogan="">
          {BRAND_SLOGAN}
        </p>
        <h1 className="nd-connect-title">{t("connect.title")}</h1>
        {linkInvalid ? (
          <p className="nd-connect-note" role="status" data-connect-link-invalid="">
            {t("connect.linkInvalid")}
          </p>
        ) : null}
        <p className="nd-connect-text">{t("connect.intro")}</p>
        <form className="nd-connect-form" onSubmit={(e) => void submit(e)} noValidate>
          <label className="nd-connect-label" htmlFor={inputId}>
            {t("connect.codeLabel")}
          </label>
          <input
            ref={inputRef}
            id={inputId}
            className="nd-input nd-connect-input nd-num"
            type="text"
            inputMode="numeric"
            pattern="[0-9]*"
            autoComplete="one-time-code"
            maxLength={7}
            autoFocus
            value={code}
            aria-invalid={error !== null}
            aria-describedby={error !== null ? errorId : undefined}
            onChange={(e) => {
              setCode(codeDigits(e.target.value));
              if (error !== null) setError(null);
            }}
            data-connect-code=""
          />
          <button type="submit" className="nd-btn nd-connect-submit" data-variant="primary" disabled={sending} data-connect-submit="">
            {sending ? t("connect.sending") : t("connect.submit")}
            {sending ? null : <Icon name="arrow-right" size={16} />}
          </button>
          {error !== null ? (
            <p id={errorId} role="alert" className="nd-connect-error" data-connect-error="">
              {error}
            </p>
          ) : null}
        </form>
        <p className="nd-connect-hint" data-connect-this-computer="">
          {t("connect.thisComputer")}
        </p>
      </div>
    </main>
    </div>
  );
}
