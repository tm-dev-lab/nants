/*
 * Passo "Adicionar dispositivos" do Acesso pela rede (spec 112, decisões 3, 4 e 8; T02).
 *
 * - "Adicionar dispositivo" abre a janela de adicionar no servidor (`POST /api/devices/code`) e
 *   mostra o código grande, a contagem até a troca (relida sozinha a cada troca), o endereço de
 *   entrada com o QR code e "Não abriu no outro aparelho?". "Fechar código" fecha a janela; sair
 *   das Configurações também.
 * - O aviso de tentativas erradas chega pelo evento `device_code_locked`; o acerto, pelo
 *   `devices_changed`, que também relê a lista.
 * - A lista mostra nome, quando entrou e o último acesso, com "Este dispositivo" marcado, e as
 *   ações de renomear e revogar (com confirmação). "Revogar todos" fica discreto, no fim.
 * - Revogar o próprio aparelho volta para `/`: na rede, o servidor leva à página de entrada.
 */
import { useCallback, useEffect, useId, useRef, useState, type FormEvent } from "react";
import type { ApiFailure } from "../../api/client";
import { subscribeDeviceEvents } from "../../api/deviceEvents";
import {
  closeDeviceCode,
  getDeviceCode,
  listDevices,
  openDeviceCode,
  renameDevice,
  revokeAllDevices,
  revokeDevice,
  type CodeWindow,
  type DeviceView,
} from "../../api/devices";
import { Dialog } from "../../desktop/Dialog";
import { Icon } from "../../desktop/Icon";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import { canAddDevice, groupedCode, type JourneyStep } from "./devicesModel";
import { QrCode } from "./QrCode";

/** Relógio e navegação (substituíveis nos testes). */
export const devicesEnvironment = {
  now: (): number => Date.now(),
  tickMs: 1000,
  /** Depois de revogar o próprio aparelho: recarrega pela raiz (na rede, vira a página de entrada). */
  leave: (): void => window.location.assign("/"),
};

type Panel =
  | { kind: "idle" }
  | { kind: "opening" }
  | { kind: "open"; win: CodeWindow; deadline: number }
  | { kind: "locked"; win: CodeWindow | null }
  | { kind: "expired" }
  | { kind: "added" }
  | { kind: "error"; message: string };

function secondsLeft(deadline: number): number {
  return Math.max(0, Math.ceil((deadline - devicesEnvironment.now()) / 1000));
}

function openPanel(win: CodeWindow): Panel {
  return { kind: "open", win, deadline: devicesEnvironment.now() + win.secondsToNext * 1000 };
}

/** Endereços de entrada: o escolhido vai para o QR; os outros ficam listados para trocar. */
function EntryAddress({ urls, lanPort }: { urls: readonly string[]; lanPort: number }) {
  const { t } = useT();
  const [chosen, setChosen] = useState(0);
  const url = urls[chosen] ?? urls[0];
  if (url === undefined) {
    return (
      <p className="nd-field-hint" data-device-no-address="">
        {t("devices.code.noAddress", { port: lanPort })}
      </p>
    );
  }
  const others = urls.map((u, i) => ({ u, i })).filter(({ i }) => i !== chosen);
  return (
    <div className="nd-device-entry">
      <QrCode value={url} label={t("devices.code.qr", { url })} />
      <div className="nd-device-entry-text">
        <span className="nd-label">{t("devices.code.address")}</span>
        <span className="nd-device-url nd-break" data-device-entry-url="">
          {url}
        </span>
        {others.length > 0 ? (
          <>
            <span className="nd-label">{t("devices.code.otherAddresses")}</span>
            <ul className="nd-device-other-urls">
              {others.map(({ u, i }) => (
                <li key={u}>
                  <button type="button" className="nd-link-btn nd-break" aria-label={t("devices.code.useAddress", { url: u })} onClick={() => setChosen(i)}>
                    {u}
                  </button>
                </li>
              ))}
            </ul>
          </>
        ) : null}
      </div>
    </div>
  );
}

/** "Não abriu no outro aparelho?": as causas comuns, recolhidas. */
function NotOpening() {
  const { t } = useT();
  return (
    <details className="nd-details nd-device-help" data-device-help="">
      <summary>
        <Icon name="chevron-right" size={14} className="nd-details-chevron" />
        {t("devices.help.title")}
      </summary>
      <ul className="nd-device-help-list">
        <li>{t("devices.help.otherNetwork")}</li>
        <li>{t("devices.help.isolation")}</li>
        <li>{t("devices.help.step2")}</li>
        <li>{t("devices.help.address")}</li>
      </ul>
      <p className="nd-field-hint">{t("devices.help.proof")}</p>
    </details>
  );
}

function CodePanel({ panel, lanPort, onClose, onNewCode }: { panel: Panel; lanPort: number; onClose: () => void; onNewCode: () => void }) {
  const { t } = useT();
  const [, setTick] = useState(0);
  useEffect(() => {
    if (panel.kind !== "open") return;
    const timer = setInterval(() => setTick((n) => n + 1), devicesEnvironment.tickMs);
    return () => clearInterval(timer);
  }, [panel.kind]);
  if (panel.kind === "idle" || panel.kind === "opening") return null;
  if (panel.kind === "error") {
    return (
      <p role="alert" className="nd-field-error" data-device-code-error="">
        {panel.message}
      </p>
    );
  }
  if (panel.kind === "added" || panel.kind === "expired" || panel.kind === "locked") {
    const text = panel.kind === "added" ? t("devices.code.added") : panel.kind === "expired" ? t("devices.code.expired") : t("devices.code.locked");
    return (
      <div className="nd-device-code" data-device-code-state={panel.kind}>
        <p className={panel.kind === "locked" ? "nd-field-warn" : "nd-field-hint"} role={panel.kind === "locked" ? "alert" : "status"}>
          {text}
        </p>
        {panel.kind === "added" ? null : (
          <div className="nd-row">
            <button type="button" className="nd-btn nd-btn-sm" data-device-new-code="" onClick={onNewCode}>
              <Icon name="refresh" size={14} />
              {t("devices.code.newCode")}
            </button>
          </div>
        )}
      </div>
    );
  }
  const code = panel.win.code ?? "";
  return (
    <div className="nd-device-code" data-device-code-state="open" role="group" aria-label={t("devices.code.title")}>
      <p className="nd-field-hint">{t("devices.code.intro")}</p>
      <div className="nd-device-code-row">
        <span className="nd-label">{t("devices.code.label")}</span>
        <span className="nd-device-digits nd-num" data-device-code={code} aria-label={code.split("").join(" ")}>
          {groupedCode(code)}
        </span>
        <span className="nd-muted nd-num" data-device-countdown="" aria-live="off">
          {t("devices.code.next", { seconds: secondsLeft(panel.deadline) })}
        </span>
      </div>
      <EntryAddress urls={panel.win.entryUrls} lanPort={lanPort} />
      <div className="nd-row">
        <button type="button" className="nd-btn nd-btn-sm" data-device-code-close="" onClick={onClose}>
          <Icon name="close" size={14} />
          {t("devices.code.close")}
        </button>
      </div>
      <NotOpening />
    </div>
  );
}

function DeviceRow({ device, busy, onRename, onRevoke }: { device: DeviceView; busy: boolean; onRename: (name: string) => Promise<boolean>; onRevoke: () => void }) {
  const { t, formatDateTime } = useT();
  const [editing, setEditing] = useState(false);
  const [name, setName] = useState(device.name);
  const inputId = useId();
  const when = (ms: number) => formatDateTime(ms, { dateStyle: "short", timeStyle: "short" });
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const clean = name.trim();
    if (clean === "" || clean === device.name) {
      setEditing(false);
      return;
    }
    if (await onRename(clean)) setEditing(false);
  };
  return (
    <li className="nd-device-row" data-device={device.id} data-device-current={device.current ? "true" : undefined}>
      {editing ? (
        <form className="nd-device-rename" onSubmit={(e) => void submit(e)}>
          <label className="nd-sr-only" htmlFor={inputId}>
            {t("devices.list.nameLabel")}
          </label>
          <input
            id={inputId}
            className="nd-input"
            value={name}
            maxLength={40}
            autoFocus
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                setName(device.name);
                setEditing(false);
              }
            }}
            data-device-name-input=""
          />
          <button type="submit" className="nd-btn nd-btn-sm" data-variant="primary" disabled={busy}>
            {t("devices.list.save")}
          </button>
          <button
            type="button"
            className="nd-btn nd-btn-sm"
            onClick={() => {
              setName(device.name);
              setEditing(false);
            }}
          >
            {t("common.cancel")}
          </button>
        </form>
      ) : (
        <div className="nd-device-main">
          <span className="nd-device-name nd-strong" data-device-name="">
            {device.name}
          </span>
          {device.current ? (
            <span className="nd-pill nd-pill-sm nd-pill-nowrap" data-tone="cyan" data-device-this="">
              {t("devices.list.current")}
            </span>
          ) : null}
          <span className="nd-device-actions">
            <button
              type="button"
              className="nd-btn nd-btn-sm"
              data-variant="ghost"
              aria-label={t("devices.list.renameFor", { name: device.name })}
              title={t("devices.list.renameFor", { name: device.name })}
              disabled={busy}
              onClick={() => {
                setName(device.name);
                setEditing(true);
              }}
            >
              <Icon name="pencil" size={14} />
            </button>
            <button
              type="button"
              className="nd-btn nd-btn-sm"
              data-variant="ghost"
              aria-label={t("devices.list.revokeFor", { name: device.name })}
              title={t("devices.list.revokeFor", { name: device.name })}
              disabled={busy}
              onClick={onRevoke}
            >
              <Icon name="trash" size={14} />
            </button>
          </span>
        </div>
      )}
      <span className="nd-muted" data-device-meta="">
        {t("devices.list.joined", { date: when(device.createdAt) })}, {t("devices.list.lastSeen", { date: when(device.lastSeen) })}
      </span>
    </li>
  );
}

type Confirm = { kind: "one"; device: DeviceView } | { kind: "all" };

function ConfirmRevoke({ confirm, onCancel, onConfirm }: { confirm: Confirm; onCancel: () => void; onConfirm: () => void }) {
  const { t } = useT();
  const titleId = useId();
  const descId = useId();
  const cancelRef = useRef<HTMLButtonElement>(null);
  const title = confirm.kind === "all" ? t("devices.revokeAll.title") : t("devices.revoke.title", { name: confirm.device.name });
  const text = confirm.kind === "all" ? t("devices.revokeAll.text") : confirm.device.current ? t("devices.revoke.textCurrent") : t("devices.revoke.text");
  return (
    <Dialog titleId={titleId} title={title} describedById={descId} onClose={onCancel} width={440} initialFocus={cancelRef} alert>
      <div id={descId} className="nd-dialog-text">
        <p>{text}</p>
      </div>
      <div className="nd-dialog-actions">
        <button ref={cancelRef} type="button" className="nd-btn" onClick={onCancel}>
          {t("common.cancel")}
        </button>
        <button type="button" className="nd-btn" data-variant="danger" data-device-confirm="" onClick={onConfirm}>
          <Icon name="trash" size={14} />
          {confirm.kind === "all" ? t("devices.revokeAll.confirm") : t("devices.revoke.confirm")}
        </button>
      </div>
    </Dialog>
  );
}

type ListLoad = { status: "loading" } | { status: "error" } | { status: "ok"; devices: DeviceView[] };

/** Lista de dispositivos autorizados, com renomear e revogar. */
function DeviceList({ load, onChanged }: { load: ListLoad; onChanged: () => void }) {
  const { t } = useT();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<Confirm | null>(null);
  const fail = (failure: ApiFailure) => setError(t("devices.actionFailed", { message: apiFailureText(failure) }));

  const rename = async (id: string, name: string): Promise<boolean> => {
    setBusy(true);
    setError(null);
    const result = await renameDevice(id, name);
    setBusy(false);
    if (!result.ok) {
      fail(result.error);
      if (result.error.code === "device_not_found") onChanged();
      return false;
    }
    onChanged();
    return true;
  };

  const revoke = async (target: Confirm) => {
    setConfirm(null);
    setBusy(true);
    setError(null);
    const devices = load.status === "ok" ? load.devices : [];
    const self = target.kind === "all" ? devices.some((d) => d.current) : target.device.current;
    const result = target.kind === "all" ? await revokeAllDevices() : await revokeDevice(target.device.id);
    setBusy(false);
    if (!result.ok) {
      fail(result.error);
      onChanged();
      return;
    }
    if (self) {
      devicesEnvironment.leave();
      return;
    }
    onChanged();
  };

  if (load.status === "loading") return null;
  if (load.status === "error") {
    return (
      <p className="nd-muted" data-device-list-failed="">
        {t("devices.list.loadFailed")}
      </p>
    );
  }
  return (
    <div className="nd-device-list-wrap" data-device-list="">
      <h4 className="nd-label">{t("devices.list.title")}</h4>
      {load.devices.length === 0 ? (
        <p className="nd-muted" data-device-list-empty="">
          {t("devices.list.empty")}
        </p>
      ) : (
        <ul className="nd-device-list" aria-label={t("devices.list.title")}>
          {load.devices.map((d) => (
            <DeviceRow key={d.id} device={d} busy={busy} onRename={(name) => rename(d.id, name)} onRevoke={() => setConfirm({ kind: "one", device: d })} />
          ))}
        </ul>
      )}
      {error !== null ? (
        <p role="alert" className="nd-field-error" data-device-error="">
          {error}
        </p>
      ) : null}
      {load.devices.length > 0 ? (
        <div className="nd-row nd-device-revoke-all">
          <button type="button" className="nd-link-btn" disabled={busy} data-device-revoke-all="" onClick={() => setConfirm({ kind: "all" })}>
            {t("devices.list.revokeAll")}
          </button>
        </div>
      ) : null}
      {confirm !== null ? <ConfirmRevoke confirm={confirm} onCancel={() => setConfirm(null)} onConfirm={() => void revoke(confirm)} /> : null}
    </div>
  );
}

/** Corpo do passo de adicionar: o botão (ou o motivo), o código e a lista. */
export function DevicesStep({ step, lanPort, reasonId }: { step: JourneyStep; lanPort: number; reasonId: string }) {
  const { t } = useT();
  const enabled = canAddDevice(step);
  const [panel, setPanel] = useState<Panel>({ kind: "idle" });
  const [list, setList] = useState<ListLoad>({ status: "loading" });
  const alive = useRef(true);
  const panelRef = useRef<Panel>(panel);
  panelRef.current = panel;
  const refreshing = useRef(false);

  const reloadList = useCallback(async () => {
    const result = await listDevices();
    if (!alive.current) return;
    setList(result.ok ? { status: "ok", devices: result.data } : { status: "error" });
  }, []);

  /** Relê a janela: aberta (código novo), fechada (expirou ou o aparelho entrou) ou invalidada. */
  const refreshCode = useCallback(async (closedAs: "expired" | "added") => {
    if (refreshing.current) return;
    refreshing.current = true;
    const result = await getDeviceCode();
    refreshing.current = false;
    if (!alive.current) return;
    const current = panelRef.current;
    if (current.kind !== "open" && current.kind !== "opening") return;
    if (!result.ok) return;
    const win = result.data;
    if (win.state === "open") setPanel(openPanel(win));
    else if (win.state === "locked") setPanel({ kind: "locked", win });
    else setPanel({ kind: closedAs });
  }, []);

  const open = useCallback(async () => {
    setPanel({ kind: "opening" });
    const result = await openDeviceCode();
    if (!alive.current) return;
    if (!result.ok) {
      setPanel({ kind: "error", message: t("devices.code.failed", { message: apiFailureText(result.error) }) });
      return;
    }
    setPanel(result.data.state === "open" ? openPanel(result.data) : { kind: "locked", win: result.data });
  }, [t]);

  const close = useCallback(() => {
    setPanel({ kind: "idle" });
    void closeDeviceCode();
  }, []);

  // Montagem: a lista e uma janela já aberta (outra aba ou a página recarregada).
  useEffect(() => {
    alive.current = true;
    void reloadList();
    void getDeviceCode().then((result) => {
      if (!alive.current || !result.ok || panelRef.current.kind !== "idle") return;
      if (result.data.state === "open") setPanel(openPanel(result.data));
      else if (result.data.state === "locked") setPanel({ kind: "locked", win: result.data });
    });
    return () => {
      alive.current = false;
      // Sair das Configurações fecha a janela de adicionar.
      if (panelRef.current.kind === "open") void closeDeviceCode();
    };
  }, [reloadList]);

  // Eventos: tentativas erradas e mudanças na lista (o acerto do código fecha a janela).
  useEffect(
    () =>
      subscribeDeviceEvents((event) => {
        if (event.event_type === "device_code_locked") {
          if (panelRef.current.kind === "open" || panelRef.current.kind === "opening") setPanel({ kind: "locked", win: null });
          return;
        }
        void reloadList();
        if (event.payload.reason === "added") void refreshCode("added");
      }),
    [reloadList, refreshCode],
  );

  // Contagem: ao chegar a zero, relê o código (o servidor troca a cada minuto).
  useEffect(() => {
    if (panel.kind !== "open") return;
    const timer = setInterval(() => {
      const current = panelRef.current;
      if (current.kind === "open" && secondsLeft(current.deadline) <= 0) void refreshCode("expired");
    }, devicesEnvironment.tickMs);
    return () => clearInterval(timer);
  }, [panel.kind, refreshCode]);

  const showButton = panel.kind === "idle" || panel.kind === "opening" || panel.kind === "error" || panel.kind === "added";
  return (
    <div className="nd-device-step" data-devices="">
      {showButton ? (
        <div className="nd-row">
          <button
            type="button"
            className="nd-btn nd-btn-sm"
            data-variant="primary"
            disabled={!enabled || panel.kind === "opening"}
            aria-describedby={reasonId}
            data-device-add=""
            onClick={() => void open()}
          >
            <Icon name="plus" size={14} />
            {t("devices.add")}
          </button>
        </div>
      ) : null}
      <CodePanel panel={panel} lanPort={lanPort} onClose={close} onNewCode={() => void open()} />
      <DeviceList load={list} onChanged={() => void reloadList()} />
    </div>
  );
}
