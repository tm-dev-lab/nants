import { describe, expect, it } from "vitest";
import { gestureRect, gestureSignal, SNAP_THRESHOLD, snapAnnouncement, snapPreviewRect, snapZoneAt } from "./gestures";

const ws = { w: 1000, h: 700 };
const start = { x: 100, y: 100, w: 400, h: 300 };

describe("gestureRect", () => {
  it("mover fica dentro da área", () => {
    expect(gestureRect(start, "move", 50, 20, ws)).toEqual({ x: 150, y: 120, w: 400, h: 300 });
    expect(gestureRect(start, "move", -500, 900, ws)).toEqual({ x: 0, y: 400, w: 400, h: 300 });
  });

  it("bordas direita, inferior e canto; mínimo 320×220 e limite da área", () => {
    expect(gestureRect(start, "resize-e", 100, 100, ws)).toEqual({ ...start, w: 500 });
    expect(gestureRect(start, "resize-s", 100, 100, ws)).toEqual({ ...start, h: 400 });
    expect(gestureRect(start, "resize-se", -300, -300, ws)).toEqual({ ...start, w: 320, h: 220 });
    expect(gestureRect(start, "resize-se", 5000, 5000, ws)).toEqual({ ...start, w: 900, h: 600 });
  });
});

describe("snapZoneAt", () => {
  it("bordas, cantos e topo a 16 px", () => {
    expect(snapZoneAt({ x: SNAP_THRESHOLD, y: 300 }, ws)).toBe("left");
    expect(snapZoneAt({ x: SNAP_THRESHOLD + 1, y: 300 }, ws)).toBeNull();
    expect(snapZoneAt({ x: 995, y: 300 }, ws)).toBe("right");
    expect(snapZoneAt({ x: 500, y: 0 }, ws)).toBe("top");
    expect(snapZoneAt({ x: 2, y: 2 }, ws)).toBe("top-left");
    expect(snapZoneAt({ x: 998, y: 2 }, ws)).toBe("top-right");
    expect(snapZoneAt({ x: 2, y: 699 }, ws)).toBe("bottom-left");
    expect(snapZoneAt({ x: 998, y: 699 }, ws)).toBe("bottom-right");
    expect(snapZoneAt({ x: 500, y: 699 }, ws)).toBeNull();
  });

  it("prévia e anúncio do encaixe", () => {
    expect(snapPreviewRect("right", start, ws)).toEqual({ x: 500, y: 0, w: 500, h: 700 });
    expect(snapAnnouncement("Conversa", "left")).toBe("Conversa encaixada: metade esquerda");
  });
});

describe("gestureSignal", () => {
  it("conta gestos simultâneos e nunca fica negativo", () => {
    gestureSignal.reset();
    gestureSignal.begin();
    gestureSignal.begin();
    gestureSignal.end();
    expect(gestureSignal.active()).toBe(true);
    gestureSignal.end();
    gestureSignal.end();
    expect(gestureSignal.active()).toBe(false);
  });
});
