/* Capturas do Revisor na janela Conversa (spec 093, T06). */
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { Conversation } from "../conversation/Conversation";
import { captureSpecOf } from "../conversation/Bubble";
import { CaptureThumbnails, captureUrl } from "./CaptureThumbnails";
import { ImageLightbox } from "./ImageLightbox";
import { parseReviewMessage, splitCaptures } from "../../reviewMessage";
import type { SpecView } from "../../api/types";
import type { ChatMessage } from "../../store";
import { makeTask, resetStore } from "../../test-utils";
import { findForbiddenGlyphs } from "../../desktop/glyphs";

afterEach(() => {
  cleanup();
  resetStore();
});

const CAPS = "\n\nCapturas avaliadas: t01-1-shots-home.png, t01-1-shots-menu.jpg";
const WITH_FINDINGS =
  "Pediu ajustes — visor sobreposto\n\nApontamentos:\n- F-01 [HIGH] src/App.tsx:10 — visor sobre o título → usar bloco\n\n" +
  "Observações (não bloqueiam):\n- F-02 [LOW] src/b.ts:1 — detalhe" +
  CAPS +
  "\n\nCritérios:\n✓ A — ok\n✗ B — sobreposição na captura t01-1-shots-home.png";
const ONLY_SUMMARY = "Aprovado — tudo certo" + CAPS + "\n\nCritérios:\n✓ A — ok";

function reviewer(text: string, extra: Partial<ChatMessage> = {}): ChatMessage {
  return { id: 1, role: "agent", agent: "reviewer", text, at: 1, origin: "live", seq: 1, taskId: "T01", attempt: 1, ...extra } as ChatMessage;
}

function renderMessage(message: ChatMessage) {
  const spec: SpecView = { id: "082-x", title: "Plano", status: "IN_PROGRESS", tasks: [makeTask("T01", "X", "IN_PROGRESS")] };
  return render(<Conversation projectId="p1" spec={spec} messages={[message]} onDraftUpdated={async () => undefined} />);
}

describe("linha de capturas na mensagem do Revisor", () => {
  it("é separada do resumo e das seções", () => {
    const a = parseReviewMessage(WITH_FINDINGS);
    expect(a?.captures).toEqual(["t01-1-shots-home.png", "t01-1-shots-menu.jpg"]);
    expect(a?.observations).toHaveLength(1);
    expect(a?.observations[0]?.description).toBe("detalhe");
    expect(a?.items).toHaveLength(2);
    const b = parseReviewMessage(ONLY_SUMMARY);
    expect(b?.summary).toBe("tudo certo");
    expect(b?.captures).toEqual(["t01-1-shots-home.png", "t01-1-shots-menu.jpg"]);
    expect(parseReviewMessage("Aprovado — ok" + CAPS)?.summary).toBe("ok");
    expect(parseReviewMessage("Aprovado — ok")?.captures).toEqual([]);
  });

  it("descarta nomes inválidos e só reconhece a linha final", () => {
    expect(splitCaptures("x\n\nCapturas avaliadas: ../a.png, a/b.png, OK.png, ok.png, c.gif").captures).toEqual(["ok.png"]);
    const middle = "x\n\nCapturas avaliadas: a.png\nmais texto";
    expect(splitCaptures(middle)).toEqual({ rest: middle, captures: [] });
  });

  it.each([
    ["com apontamentos e observações", WITH_FINDINGS],
    ["só com resumo", ONLY_SUMMARY],
  ])("mostra miniaturas com a URL certa e sem o texto bruto (%s)", (_label, text) => {
    const { container } = renderMessage(reviewer(text));
    expect(container.textContent).not.toContain("Capturas avaliadas:");
    expect(container.textContent).not.toContain("t01-1-shots-menu.jpg");
    const list = screen.getByRole("list", { name: "Capturas avaliadas" });
    const imgs = within(list).getAllByRole("img");
    expect(imgs.map((i) => i.getAttribute("alt"))).toEqual(["t01-1-shots-home.png", "t01-1-shots-menu.jpg"]);
    expect(imgs[0]?.getAttribute("src")).toBe("/api/projects/p1/specs/082-x/captures/t01-1-shots-home.png");
    expect(imgs.every((i) => i.getAttribute("loading") === "lazy")).toBe(true);
    expect(imgs[0]?.className).toContain("nd-thumb-img");
    expect(findForbiddenGlyphs(container)).toEqual([]);
    // O checklist de critérios continua.
    const criteria = screen.getByRole("list", { name: "Critérios de aceitação" });
    expect(within(criteria).getAllByRole("listitem").length).toBeGreaterThan(0);
  });

  it("usa a spec da mensagem quando conhecida", () => {
    renderMessage(reviewer(ONLY_SUMMARY, { specId: "082-x" }));
    const img = screen.getByRole("img", { name: "t01-1-shots-home.png" });
    expect(img.getAttribute("src")).toBe("/api/projects/p1/specs/082-x/captures/t01-1-shots-home.png");
    expect(captureSpecOf(reviewer("x", { specId: "081-outra" }), "ativa")).toBe("081-outra");
    expect(captureSpecOf(reviewer("x"), "ativa")).toBe("ativa");
    expect(captureSpecOf(reviewer("x", { unscoped: true, arrivedUnderSpec: "chegada" }), "ativa")).toBe("chegada");
    expect(captureSpecOf(reviewer("x"), null)).toBeNull();
  });
});

describe("CaptureThumbnails", () => {
  it("limita a 8 miniaturas e codifica a URL", () => {
    const names = Array.from({ length: 10 }, (_, i) => `c${i}.png`);
    render(<CaptureThumbnails projectId="p 1" specId="s1" names={names} />);
    expect(screen.getAllByRole("img")).toHaveLength(8);
    expect(captureUrl("p 1", "s1", "a.png")).toBe("/api/projects/p%201/specs/s1/captures/a.png");
  });

  it("abre o ampliado e fecha com Esc e com o botão Fechar", () => {
    render(<CaptureThumbnails projectId="p1" specId="s1" names={["a.png", "b.png"]} />);
    const thumb = screen.getByRole("button", { name: "Ampliar captura a.png" });
    thumb.focus();
    fireEvent.click(thumb);
    let dialog = screen.getByRole("dialog", { name: "a.png" });
    expect(within(dialog).getByRole("img", { name: "a.png" }).getAttribute("src")).toBe("/api/projects/p1/specs/s1/captures/a.png");
    fireEvent.keyDown(dialog, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(thumb);
    fireEvent.click(screen.getByRole("button", { name: "Ampliar captura b.png" }));
    dialog = screen.getByRole("dialog", { name: "b.png" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Fechar" }));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("imagem que falha ao carregar vira o nome em texto", () => {
    render(<CaptureThumbnails projectId="p1" specId="s1" names={["a.png", "b.png"]} />);
    fireEvent.error(screen.getByRole("img", { name: "a.png" }));
    expect(screen.queryByRole("img", { name: "a.png" })).toBeNull();
    expect(screen.getByTestId("capture-failed").textContent).toBe("a.png");
    expect(screen.getByRole("img", { name: "b.png" })).toBeTruthy();
  });

  it("o ampliado que falha mostra o nome em texto", () => {
    render(<ImageLightbox src="/x.png" alt="x.png" onClose={() => {}} />);
    fireEvent.error(screen.getByRole("img", { name: "x.png" }));
    expect(screen.getByRole("status").textContent).toContain("x.png");
  });
});
