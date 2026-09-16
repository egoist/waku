/**
 * Multiply a CSS-style color's alpha for the streaming veil. The veil only
 * ever dims paint colors — layout never changes — so an unparseable color
 * falls back to itself rather than breaking rendering.
 */

type Parsed =
  | { kind: 'rgb'; r: number; g: number; b: number; a: number }
  | { kind: 'hsl'; h: string; s: string; l: string; a: number }
  | null;

const cache = new Map<string, Parsed>();

function parse(color: string): Parsed {
  const hex = /^#([0-9a-f]{3,8})$/i.exec(color)?.[1];
  if (hex && [3, 4, 6, 8].includes(hex.length)) {
    const wide = hex.length <= 4
      ? [...hex].map((digit) => digit + digit).join('')
      : hex;
    const value = Number.parseInt(wide, 16);
    if (wide.length === 8) {
      return {
        kind: 'rgb',
        r: (value >>> 24) & 0xff,
        g: (value >>> 16) & 0xff,
        b: (value >>> 8) & 0xff,
        a: (value & 0xff) / 255,
      };
    }
    return {
      kind: 'rgb',
      r: (value >>> 16) & 0xff,
      g: (value >>> 8) & 0xff,
      b: value & 0xff,
      a: 1,
    };
  }
  const fn = /^(rgba?|hsla?)\(\s*([^)]+)\)$/i.exec(color);
  if (fn) {
    const parts = fn[2]!.split(/[,/]/).map((part) => part.trim()).filter(Boolean);
    if (parts.length < 3) return null;
    const alpha = parts.length > 3 ? Number.parseFloat(parts[3]!) : 1;
    if (Number.isNaN(alpha)) return null;
    if (fn[1]!.toLowerCase().startsWith('rgb')) {
      const [r, g, b] = parts.map((part) => Number.parseFloat(part));
      if ([r, g, b].some((channel) => Number.isNaN(channel!))) return null;
      return { kind: 'rgb', r: r!, g: g!, b: b!, a: alpha };
    }
    return { kind: 'hsl', h: parts[0]!, s: parts[1]!, l: parts[2]!, a: alpha };
  }
  return null;
}

export function applyAlpha(color: string, factor: number): string {
  if (factor >= 1) return color;
  let parsed = cache.get(color);
  if (parsed === undefined) {
    parsed = parse(color);
    cache.set(color, parsed);
  }
  if (!parsed) return color;
  const alpha = Math.max(0, Math.min(1, parsed.a * factor));
  if (parsed.kind === 'rgb') {
    return `rgba(${parsed.r}, ${parsed.g}, ${parsed.b}, ${round(alpha)})`;
  }
  return `hsla(${parsed.h}, ${parsed.s}, ${parsed.l}, ${round(alpha)})`;
}

function round(alpha: number): number {
  return Math.round(alpha * 1_000) / 1_000;
}
