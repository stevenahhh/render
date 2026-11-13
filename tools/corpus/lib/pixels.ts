import { inflateSync } from "node:zlib";

export const SMALL_W = 320;
export const SMALL_H = 200;

export type Gray = { readonly w: number; readonly h: number; readonly px: Float64Array };
type Raw = { readonly w: number; readonly h: number; readonly channels: number; readonly data: Uint8Array };

// Reads only what Bun.Image's PNG encoder writes: 8-bit, non-interlaced, gray/RGB(A).
function decodePng(png: Uint8Array): Raw {
  const view = new DataView(png.buffer, png.byteOffset, png.byteLength);
  let pos = 8;
  let w = 0;
  let h = 0;
  let channels = 0;
  const idat: Uint8Array[] = [];
  while (pos < png.length) {
    const len = view.getUint32(pos);
    const type = String.fromCharCode(...png.subarray(pos + 4, pos + 8));
    const data = png.subarray(pos + 8, pos + 8 + len);
    if (type === "IHDR") {
      w = view.getUint32(pos + 8);
      h = view.getUint32(pos + 12);
      const [depth, color, , , interlace] = data.subarray(8, 13);
      channels = ({ 0: 1, 2: 3, 4: 2, 6: 4 } as Record<number, number>)[color] ?? 0;
      if (depth !== 8 || channels === 0 || interlace !== 0) throw new Error(`unsupported PNG: depth ${depth} color ${color} interlace ${interlace}`);
    } else if (type === "IDAT") idat.push(data);
    pos += 12 + len;
  }
  const raw = inflateSync(Buffer.concat(idat));
  const stride = w * channels;
  const out = new Uint8Array(stride * h);
  for (let y = 0; y < h; y++) {
    const filter = raw[y * (stride + 1)];
    const src = y * (stride + 1) + 1;
    const row = y * stride;
    for (let i = 0; i < stride; i++) {
      const x = raw[src + i];
      const a = i >= channels ? out[row + i - channels] : 0;
      const b = y > 0 ? out[row - stride + i] : 0;
      const c = i >= channels && y > 0 ? out[row - stride + i - channels] : 0;
      let v = x;
      if (filter === 1) v = x + a;
      else if (filter === 2) v = x + b;
      else if (filter === 3) v = x + ((a + b) >> 1);
      else if (filter === 4) {
        const p = a + b - c;
        const pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
        v = x + (pa <= pb && pa <= pc ? a : pb <= pc ? b : c);
      }
      out[row + i] = v & 255;
    }
  }
  return { w, h, channels, data: out };
}

function rgbAt(r: Raw, i: number): [number, number, number] {
  const o = i * r.channels;
  return r.channels >= 3 ? [r.data[o], r.data[o + 1], r.data[o + 2]] : [r.data[o], r.data[o], r.data[o]];
}

export async function small(path: string): Promise<Gray> {
  const raw = decodePng(await new Bun.Image(Bun.file(path)).resize(SMALL_W, SMALL_H, { fit: "fill" }).png().bytes());
  const px = new Float64Array(raw.w * raw.h);
  for (let i = 0; i < px.length; i++) {
    const [r, g, b] = rgbAt(raw, i);
    px[i] = 0.299 * r + 0.587 * g + 0.114 * b;
  }
  return { w: raw.w, h: raw.h, px };
}

// Mean SSIM over non-overlapping 8x8 windows; c1, c2 are Wang et al. (2004) for 8-bit data.
export function ssim(a: Gray, b: Gray): number {
  const c1 = (0.01 * 255) ** 2;
  const c2 = (0.03 * 255) ** 2;
  let total = 0;
  let n = 0;
  for (let y0 = 0; y0 + 8 <= a.h; y0 += 8) {
    for (let x0 = 0; x0 + 8 <= a.w; x0 += 8) {
      let sa = 0, sb = 0, saa = 0, sbb = 0, sab = 0;
      for (let y = y0; y < y0 + 8; y++) {
        for (let x = x0; x < x0 + 8; x++) {
          const p = a.px[y * a.w + x];
          const q = b.px[y * b.w + x];
          sa += p; sb += q; saa += p * p; sbb += q * q; sab += p * q;
        }
      }
      const ma = sa / 64, mb = sb / 64;
      const va = saa / 64 - ma * ma, vb = sbb / 64 - mb * mb, cov = sab / 64 - ma * mb;
      total += ((2 * ma * mb + c1) * (2 * cov + c2)) / ((ma * ma + mb * mb + c1) * (va + vb + c2));
      n++;
    }
  }
  return n === 0 ? 0 : total / n;
}

export type Diff = { readonly pixels: number; readonly maxDiff: number };

export async function exactDiff(pathA: string, pathB: string): Promise<Diff> {
  const read = async (p: string) => decodePng(await new Bun.Image(Bun.file(p)).png().bytes());
  const [a, b] = await Promise.all([read(pathA), read(pathB)]);
  if (a.w !== b.w || a.h !== b.h) return { pixels: a.w * a.h, maxDiff: 255 };
  let pixels = 0;
  let maxDiff = 0;
  for (let i = 0; i < a.w * a.h; i++) {
    const [r1, g1, b1] = rgbAt(a, i);
    const [r2, g2, b2] = rgbAt(b, i);
    const d = Math.max(Math.abs(r1 - r2), Math.abs(g1 - g2), Math.abs(b1 - b2));
    if (d > 0) {
      pixels++;
      maxDiff = Math.max(maxDiff, d);
    }
  }
  return { pixels, maxDiff };
}
