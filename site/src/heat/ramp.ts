/** The thermal ramp, cold to hot. style.css names the same colors. */
const STOPS: ReadonlyArray<readonly [number, string]> = [
  [0.0, "#04030f"],
  [0.07, "#06051a"],
  [0.17, "#100a33"],
  [0.31, "#3b1178"],
  [0.46, "#a3167f"],
  [0.58, "#d4245e"],
  [0.71, "#ff6a1f"],
  [0.85, "#ffc53d"],
  [1.0, "#fff6dc"],
];

type Triple = [number, number, number];

function srgbToLinear(c: number): number {
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}

function linearToSrgb(c: number): number {
  const v = c <= 0.0031308 ? c * 12.92 : 1.055 * c ** (1 / 2.4) - 0.055;
  return Math.min(1, Math.max(0, v));
}

function hexToOklab(hex: string): Triple {
  const n = Number.parseInt(hex.slice(1), 16);
  const r = srgbToLinear(((n >> 16) & 255) / 255);
  const g = srgbToLinear(((n >> 8) & 255) / 255);
  const b = srgbToLinear((n & 255) / 255);
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}

function oklabToSrgb([L, a, b]: Triple): Triple {
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    linearToSrgb(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
    linearToSrgb(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
    linearToSrgb(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s),
  ];
}

/** RGBA bytes of the ramp, `size` pixels wide, interpolated in OKLab. */
export function rampPixels(size = 256): Uint8Array {
  const stops = STOPS.map(([at, hex]) => ({ at, lab: hexToOklab(hex) }));
  const out = new Uint8Array(size * 4);
  let seg = 0;
  for (let i = 0; i < size; i++) {
    const t = i / (size - 1);
    while (seg < stops.length - 2 && t > stops[seg + 1]!.at) seg++;
    const lo = stops[seg]!;
    const hi = stops[seg + 1]!;
    const k = Math.min(1, Math.max(0, (t - lo.at) / (hi.at - lo.at)));
    const lab: Triple = [
      lo.lab[0] + (hi.lab[0] - lo.lab[0]) * k,
      lo.lab[1] + (hi.lab[1] - lo.lab[1]) * k,
      lo.lab[2] + (hi.lab[2] - lo.lab[2]) * k,
    ];
    const [r, g, b] = oklabToSrgb(lab);
    out[i * 4] = Math.round(r * 255);
    out[i * 4 + 1] = Math.round(g * 255);
    out[i * 4 + 2] = Math.round(b * 255);
    out[i * 4 + 3] = 255;
  }
  return out;
}
