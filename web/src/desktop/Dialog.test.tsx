/*
 * Diálogo modal do desktop: foco inicial no primeiro controle (ou no pedido), Tab preso no painel,
 * Esc fecha sem chegar aos atalhos, clique no fundo fecha (dentro do painel, não) e o foco volta a
 * quem abriu.
 */
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useRef, useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Dialog } from "./Dialog";

afterEach(cleanup);

function Harness({ onClose = () => undefined, focusSecond = false }: { onClose?: () => void; focusSecond?: boolean }) {
  const [open, setOpen] = useState(false);
  const second = useRef<HTMLButtonElement | null>(null);
  return (
    <div>
      <button type="button" onClick={() => setOpen(true)}>
        Abrir
      </button>
      {open ? (
        <Dialog
          titleId="t"
          title="Título"
          onClose={() => {
            onClose();
            setOpen(false);
          }}
          {...(focusSecond ? { initialFocus: second } : {})}
        >
          <input aria-label="Campo" />
          <button ref={second} type="button">
            Último
          </button>
        </Dialog>
      ) : null}
    </div>
  );
}

function openDialog(props: Parameters<typeof Harness>[0] = {}): HTMLElement {
  render(<Harness {...props} />);
  const opener = screen.getByRole("button", { name: "Abrir" });
  opener.focus();
  fireEvent.click(opener);
  return screen.getByRole("dialog", { name: "Título" });
}

describe("Dialog", () => {
  it("foco inicial no primeiro controle; com initialFocus, no pedido", () => {
    openDialog();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Fechar" }));
    cleanup();
    openDialog({ focusSecond: true });
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Último" }));
  });

  it("Tab e Shift+Tab ciclam dentro do painel e não escapam", () => {
    const dialog = openDialog();
    const last = screen.getByRole("button", { name: "Último" });
    last.focus();
    fireEvent.keyDown(dialog, { key: "Tab" });
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Fechar" }));
    fireEvent.keyDown(dialog, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(last);
  });

  it("Esc fecha, não propaga e devolve o foco a quem abriu", () => {
    const outer = vi.fn();
    document.addEventListener("keydown", outer);
    const onClose = vi.fn();
    const dialog = openDialog({ onClose });
    fireEvent.keyDown(dialog, { key: "Escape" });
    document.removeEventListener("keydown", outer);
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(outer).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Abrir" }));
  });

  it("clique no fundo fecha; clique dentro do painel não", () => {
    const onClose = vi.fn();
    const dialog = openDialog({ onClose });
    fireEvent.mouseDown(screen.getByLabelText("Campo"));
    fireEvent.mouseDown(dialog);
    expect(onClose).not.toHaveBeenCalled();
    const backdrop = dialog.closest("[data-dialog-backdrop]");
    if (backdrop === null) throw new Error("fundo ausente");
    fireEvent.mouseDown(backdrop);
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
