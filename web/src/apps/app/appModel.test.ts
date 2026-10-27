/*
 * Regras puras da janela App (spec 093, T09): guardas do iframe, tamanhos com a escala, fase a
 * partir do estado do servidor, mescla das releituras e o resumo.
 */
import { describe, expect, it } from "vitest";
import type { AppPort, AppStatus } from "../../api/client";
import type { SpaceData } from "../../desktop/useSpaceData";
import { makeSpec } from "../../test-utils";
import {
  APP_VIEWPORTS,
  appSummary,
  createStatusSequence,
  embedDecision,
  embedProbePending,
  firstPort,
  frameGeometry,
  frameStyle,
  isLoopbackHost,
  mergeStatus,
  phaseFromStatus,
  portEmbeddable,
  reachUnverifiedNote,
  reachHintText,
  reachPortNote,
  reachRangeHint,
  reachRangeNote,
  reloadOnEmbedVerdict,
  safeHref,
  urlPort,
} from "./appModel";

const PAGE = { hostname: "192.168.0.68", origin: "http://192.168.0.68:3000" };
const PORT: AppPort = { service: "web", target: 80, published: 18080, url: "http://192.168.0.68:18080", embeddable: true };

function status(extra: Partial<AppStatus> = {}): AppStatus {
  return { state: "running", spec_id: "s1", url: null, started_at: "x", exit_code: null, ports: [PORT], notices: [], embeddable: true, network: null, mode: null, command: null, finished: null, failure: null, ...extra };
}

describe("embedDecision (guardas do iframe)", () => {
  it("embute http com o host da página e outra origem; confirmado só com embeddable true", () => {
    expect(embedDecision("http://192.168.0.68:18080", true, PAGE)).toEqual({ embed: true, confirmed: true });
    expect(embedDecision("http://192.168.0.68:18080/", null, PAGE)).toEqual({ embed: true, confirmed: false });
  });

  it("mesma origem do Nants: não embute", () => {
    expect(embedDecision("http://192.168.0.68:3000", true, PAGE)).toEqual({ embed: false, reason: "same_origin" });
    expect(embedDecision("http://192.168.0.68:3000/app", null, PAGE)).toEqual({ embed: false, reason: "same_origin" });
  });

  it("outro host: não embute (inclusive localhost quando a página é pelo IP da LAN)", () => {
    expect(embedDecision("http://evil.example:18080", true, PAGE)).toEqual({ embed: false, reason: "other_host" });
    expect(embedDecision("http://localhost:18080", true, PAGE)).toEqual({ embed: false, reason: "other_host" });
  });

  it("não http, userinfo e inválido: não embute", () => {
    expect(embedDecision("https://192.168.0.68:18080", true, PAGE)).toEqual({ embed: false, reason: "not_http" });
    expect(embedDecision("javascript:alert(1)", true, PAGE)).toEqual({ embed: false, reason: "not_http" });
    expect(embedDecision("http://u:p@192.168.0.68:18080", true, PAGE)).toEqual({ embed: false, reason: "invalid" });
    expect(embedDecision("lixo", true, PAGE)).toEqual({ embed: false, reason: "invalid" });
  });

  it("embeddable false: não embute", () => {
    expect(embedDecision("http://192.168.0.68:18080", false, PAGE)).toEqual({ embed: false, reason: "not_embeddable" });
  });

  it("IPv6 entre colchetes", () => {
    const v6 = { hostname: "[::1]", origin: "http://[::1]:3000" };
    expect(embedDecision("http://[::1]:18080", true, v6)).toEqual({ embed: true, confirmed: true });
    expect(embedDecision("http://[::1]:3000", true, v6)).toEqual({ embed: false, reason: "same_origin" });
  });

  it("safeHref: só http e https", () => {
    expect(safeHref("http://192.168.0.68:18080")).toBe("http://192.168.0.68:18080");
    expect(safeHref("javascript:alert(1)")).toBeNull();
    expect(safeHref("data:text/html,x")).toBeNull();
    expect(safeHref("lixo")).toBeNull();
  });
});

describe("iframe branco no celular (094/T05): Nants e app pelo IP da LAN", () => {
  const LAN = { hostname: "192.168.0.68", origin: "http://192.168.0.68:3000" };
  const APP = "http://192.168.0.68:5173";

  it("mesmo host e outra porta embute, com ou sem veredito do servidor (as guardas não bloqueiam)", () => {
    expect(embedDecision(APP, true, LAN)).toEqual({ embed: true, confirmed: true });
    expect(embedDecision(APP, null, LAN)).toEqual({ embed: true, confirmed: false });
    expect(embedDecision(`${APP}/`, null, LAN)).toEqual({ embed: true, confirmed: false });
  });

  it("livre não depende da largura medida (0 ou não): ocupa o palco, sem escala", () => {
    expect(frameGeometry("free", 0)).toEqual({ width: null, scale: 1 });
    expect(frameStyle(frameGeometry("free", 0))).toEqual({ width: "100%", height: "100%" });
    expect(frameGeometry("phone", 0)).toEqual({ width: 390, scale: 1 });
  });

  it("causa: o iframe abre antes de o app responder (veredito indeterminado); relê até ele responder", () => {
    const port = { ...PORT, url: APP, embeddable: null };
    expect(embedProbePending({ kind: "running", ports: [port], embeddable: null })).toBe(true);
    expect(embedProbePending({ kind: "running", ports: [{ ...port, embeddable: true }], embeddable: true })).toBe(false);
    expect(embedProbePending({ kind: "running", ports: [{ ...port, embeddable: false }], embeddable: false })).toBe(false);
    expect(embedProbePending({ kind: "running", ports: [], embeddable: null })).toBe(false);
    expect(embedProbePending({ kind: "starting" })).toBe(false);
  });

  it("o app passou a responder: recarrega o iframe uma vez (só de indeterminado para embutível)", () => {
    expect(reloadOnEmbedVerdict(null, true)).toBe(true);
    expect(reloadOnEmbedVerdict(null, null)).toBe(false);
    expect(reloadOnEmbedVerdict(null, false)).toBe(false);
    expect(reloadOnEmbedVerdict(true, true)).toBe(false);
    expect(reloadOnEmbedVerdict(undefined, true)).toBe(false);
  });
});

describe("tamanhos da tela", () => {
  it("as quatro opções na ordem", () => {
    expect(APP_VIEWPORTS.map((v) => [v.label, v.width])).toEqual([
      ["Livre", null],
      ["Desktop 1440", 1440],
      ["Tablet 768", 768],
      ["Celular 390", 390],
    ]);
  });

  it("livre ocupa a área; maior que a área escala para caber; menor fica em 1", () => {
    expect(frameGeometry("free", 800)).toEqual({ width: null, scale: 1 });
    expect(frameGeometry("desktop", 720)).toEqual({ width: 1440, scale: 0.5 });
    expect(frameGeometry("tablet", 800)).toEqual({ width: 768, scale: 1 });
    expect(frameGeometry("phone", 300)).toEqual({ width: 390, scale: 0.7692 });
    expect(frameGeometry("desktop", 0)).toEqual({ width: 1440, scale: 1 });
  });

  it("estilo: transform só com escala; a altura compensa a escala", () => {
    expect(frameStyle({ width: null, scale: 1 })).toEqual({ width: "100%", height: "100%" });
    expect(frameStyle({ width: 390, scale: 1 })).toEqual({ width: "390px", height: "100%" });
    expect(frameStyle({ width: 1440, scale: 0.5 })).toEqual({ width: "1440px", height: "200%", transform: "scale(0.5)", transformOrigin: "0 0" });
  });
});

describe("fase da execução", () => {
  it("no ar da própria especificação, de outra, encerrado e parado", () => {
    expect(phaseFromStatus(status(), "s1")).toEqual({ kind: "running", ports: [PORT], embeddable: true, mode: "page", command: null, startedAt: "x" });
    // Spec 130: rodando sem página e o comando do servidor.
    expect(phaseFromStatus(status({ mode: "running", command: "python -m worker" }), "s1")).toMatchObject({ kind: "running", mode: "running", command: "python -m worker" });
    expect(phaseFromStatus(status({ spec_id: "s0" }), "s1")).toEqual({ kind: "other" });
    expect(phaseFromStatus(status({ spec_id: "s0" }), null).kind).toBe("running");
    expect(phaseFromStatus(status({ state: "exited", exit_code: 1 }), "s1")).toEqual({ kind: "exited", code: 1, reason: null, finished: null, command: null });
    expect(phaseFromStatus(status({ state: "exited" }), null)).toEqual({ kind: "stopped" });
    expect(phaseFromStatus(status({ state: "stopped" }), "s1")).toEqual({ kind: "stopped" });
  });

  it("mescla: adota na carga inicial; completa portas no ar; não desfaz partida nem encerramento", () => {
    const next = phaseFromStatus(status(), "s1");
    expect(mergeStatus({ kind: "stopped" }, next)).toBe(next);
    const bare = { kind: "running" as const, ports: [], embeddable: null };
    expect(mergeStatus(bare, next)).toEqual(next);
    // A releitura troca o modo (running vira page) e completa o comando.
    const live = phaseFromStatus(status({ mode: "running", command: "w" }), "s1");
    expect(mergeStatus(live, next)).toMatchObject({ kind: "running", mode: "page", command: "w" });
    expect(mergeStatus(next, { kind: "running", ports: [], embeddable: null })).toBe(next);
    expect(mergeStatus({ kind: "starting" }, next)).toEqual({ kind: "starting" });
    expect(mergeStatus({ kind: "exited", code: 0, reason: "exited" }, next).kind).toBe("exited");
  });

  it("no ar e a releitura mostra parado ou encerrado: adota o do servidor (não fica preso em 'No ar')", () => {
    const running = phaseFromStatus(status(), "s1");
    const stopped = phaseFromStatus(status({ state: "stopped" }), "s1");
    const exited = phaseFromStatus(status({ state: "exited", exit_code: 137 }), "s1");
    expect(mergeStatus(running, stopped)).toEqual({ kind: "stopped" });
    expect(mergeStatus(running, exited)).toEqual({ kind: "exited", code: 137, reason: null, finished: null, command: null });
    // Sem portas ainda (logo depois do `app_started`), também.
    expect(mergeStatus({ kind: "running", ports: [], embeddable: null }, stopped)).toEqual({ kind: "stopped" });
    // Partida em curso continua protegida contra uma resposta antiga.
    expect(mergeStatus({ kind: "starting" }, stopped)).toEqual({ kind: "starting" });
  });

  it("primeira porta e o embeddable dela (ou o do app)", () => {
    const phase = phaseFromStatus(status({ embeddable: false, ports: [{ ...PORT, embeddable: null }] }), "s1");
    const port = firstPort(phase);
    expect(port?.url).toBe(PORT.url);
    expect(port === null ? undefined : portEmbeddable(phase, port)).toBe(false);
    expect(firstPort({ kind: "other" })).toBeNull();
  });
});

describe("resumo da janela", () => {
  it("sem espaço, aguardando e concluída", () => {
    expect(appSummary({ space: null, readiness: null, projectCount: 0 })).toEqual({});
    const waiting = { activeSpec: makeSpec("s1", "Loja", "IN_PROGRESS") } as unknown as SpaceData;
    expect(appSummary({ space: waiting, readiness: null, projectCount: 1 })).toEqual({ subtitle: "aguardando a conclusão" });
    const done = { activeSpec: makeSpec("s1", "Loja", "COMPLETED") } as unknown as SpaceData;
    expect(appSummary({ space: done, readiness: null, projectCount: 1 })).toEqual({ subtitle: "s1 · pronta para testar" });
  });
});

describe("ordem das leituras do estado", () => {
  it("resposta que saiu antes de uma mudança local é descartada; a seguinte vale", () => {
    const seq = createStatusSequence();
    const initial = seq.issue();
    seq.markLocal(); // partida pedida
    seq.markLocal(); // resposta do POST: "No ar"
    expect(seq.accept(initial)).toBe(false);
    const after = seq.issue();
    expect(seq.accept(after)).toBe(true);
  });

  it("fora de ordem: uma resposta mais velha que a já aplicada é descartada", () => {
    const seq = createStatusSequence();
    const first = seq.issue();
    const second = seq.issue();
    expect(seq.accept(second)).toBe(true);
    expect(seq.accept(first)).toBe(false);
  });

  it("sem mudança local, a carga inicial é aplicada", () => {
    const seq = createStatusSequence();
    expect(seq.accept(seq.issue())).toBe(true);
  });
});

describe("porta talvez não liberada na rede (095/T09)", () => {
  it("acesso local (loopback) não mostra a ajuda; o IP da LAN e nomes de rede sim", () => {
    for (const h of ["localhost", "LOCALHOST", "app.localhost", "127.0.0.1", "127.1.2.3", "::1", "[::1]"]) expect(isLoopbackHost(h)).toBe(true);
    for (const h of ["192.168.0.68", "10.0.0.2", "meu-pc.local", "172.21.154.252", "127.example.com"]) expect(isLoopbackHost(h)).toBe(false);
  });

  it("textos genéricos com a porta: sem comandos nem sistema, sem afirmar que está inalcançável, sem travessão nem emoji", () => {
    expect(urlPort("http://192.168.0.68:4173/x")).toBe("4173");
    expect(urlPort("http://192.168.0.68/")).toBe("80");
    expect(urlPort("nada")).toBeNull();
    expect(reachHintText("4173")).toBe(
      "Página em branco neste aparelho? A porta 4173 do app pode não estar liberada na rede do computador onde o Nants roda. Peça a liberação dessa porta a quem administra o computador ou a rede.",
    );
    expect(reachPortNote("4173")).toBe("Página em branco neste aparelho? A porta 4173 pode não estar liberada na rede.");
    for (const t of [reachHintText("4173"), reachPortNote("4173")]) {
      expect(t).not.toMatch(/[\u2013\u2014]|\p{Extended_Pictographic}/u);
      expect(t).not.toMatch(/não alcança|inalcançável/);
      expect(t).not.toMatch(/WSL|PowerShell|netsh|ufw|firewall|sudo|Windows|Linux|VM\b/i);
    }
  });

  it("textos da faixa (097): a faixa real, liberação única, sem comandos nem sistema", () => {
    const range = { start: 3001, end: 3010 };
    expect(reachRangeHint(range)).toBe(
      "Página em branco neste aparelho? As portas de visualização do Nants (3001 a 3010) podem não estar liberadas na rede do computador onde ele roda. Basta liberar essa faixa uma vez.",
    );
    expect(reachRangeNote(range)).toBe("Página em branco neste aparelho? As portas de visualização (3001 a 3010) podem não estar liberadas na rede.");
    expect(reachRangeHint({ start: 4101, end: 4120 })).toContain("(4101 a 4120)");
    for (const t of [reachRangeHint(range), reachRangeNote(range), reachUnverifiedNote()]) {
      expect(t).not.toMatch(/[\u2013\u2014]|\p{Extended_Pictographic}/u);
      expect(t).not.toMatch(/WSL|PowerShell|netsh|ufw|firewall|sudo|Windows|Linux|VM\b/i);
    }
    expect(reachUnverifiedNote()).toContain("não consegue verificar");
  });
});
