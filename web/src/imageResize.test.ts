import { afterEach, describe, expect, it, vi } from "vitest";
import { JPEG_QUALITY, keepsOriginal, MAX_IMAGE_SIDE, prepareImage, SMALL_PNG_BYTES, targetSize } from "./imageResize";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("redução da imagem antes do upload (spec 082 T05)", () => {
  it("maior lado vira no máximo 1600 px, mantendo a proporção", () => {
    expect(MAX_IMAGE_SIDE).toBe(1600);
    expect(targetSize(3200, 1600)).toEqual({ width: 1600, height: 800 });
    expect(targetSize(1000, 4000)).toEqual({ width: 400, height: 1600 });
    expect(targetSize(1600, 900)).toEqual({ width: 1600, height: 900 });
    expect(targetSize(300, 200)).toEqual({ width: 300, height: 200 });
  });

  it("só PNG pequeno (lado e bytes) vai sem conversão", () => {
    expect(keepsOriginal("image/png", SMALL_PNG_BYTES, 1600, 1000)).toBe(true);
    expect(keepsOriginal("image/png", SMALL_PNG_BYTES + 1, 800, 600)).toBe(false);
    expect(keepsOriginal("image/png", 1000, 1601, 600)).toBe(false);
    expect(keepsOriginal("image/jpeg", 1000, 800, 600)).toBe(false);
  });

  function stubDecode(width: number, height: number) {
    const close = vi.fn();
    vi.stubGlobal("createImageBitmap", vi.fn(async () => ({ width, height, close })));
    return close;
  }

  it("reencoda em JPEG ~0,85 via canvas no tamanho reduzido", async () => {
    const close = stubDecode(3200, 1600);
    const draw = vi.fn();
    const ctx = { fillStyle: "", fillRect: vi.fn(), drawImage: draw };
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(ctx as unknown as CanvasRenderingContext2D);
    const toBlob = vi
      .spyOn(HTMLCanvasElement.prototype, "toBlob")
      .mockImplementation(function (this: HTMLCanvasElement, cb: BlobCallback) {
        cb(new Blob([`${this.width}x${this.height}`], { type: "image/jpeg" }));
      });
    const out = await prepareImage(new Blob(["x"], { type: "image/png" }));
    expect(out.type).toBe("image/jpeg");
    expect(await out.blob.text()).toBe("1600x800");
    expect(toBlob.mock.calls[0]?.[1]).toBe("image/jpeg");
    expect(toBlob.mock.calls[0]?.[2]).toBe(JPEG_QUALITY);
    expect(draw.mock.calls[0]?.slice(1)).toEqual([0, 0, 1600, 800]);
    expect(close).toHaveBeenCalled();
  });

  it("PNG pequeno volta o mesmo arquivo", async () => {
    stubDecode(800, 600);
    const toBlob = vi.spyOn(HTMLCanvasElement.prototype, "toBlob");
    const file = new Blob(["png"], { type: "image/png" });
    const out = await prepareImage(file);
    expect(out).toEqual({ blob: file, type: "image/png" });
    expect(toBlob).not.toHaveBeenCalled();
  });
});
