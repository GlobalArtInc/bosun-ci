export type Colour = number | string;

export interface Look {
  foreground: Colour | null;
  background: Colour | null;
  bold: boolean;
  dim: boolean;
  italic: boolean;
  underline: boolean;
  strike: boolean;
  inverse: boolean;
}

interface Segment {
  start: number;
  text: string;
  look: Look;
}

interface Escape {
  end: number;
  sgr: string | null;
}

interface Extended {
  colour: Colour | null;
  used: number;
}

const ESC = '\u001b';

const BEL = '\u0007';

const PLAIN: Look = {
  foreground: null,
  background: null,
  bold: false,
  dim: false,
  italic: false,
  underline: false,
  strike: false,
  inverse: false,
};

const CUBE = [0, 95, 135, 175, 215, 255];

const PALETTE = 16;

function within(code: number, low: number, high: number): boolean {
  return code >= low && code <= high;
}

function escapeAt(text: string, start: number): Escape {
  const kind = text[start + 1];
  if (kind === '[') {
    let index = start + 2;
    while (index < text.length && within(text.charCodeAt(index), 0x30, 0x3f)) {
      index += 1;
    }
    const parameters = text.slice(start + 2, index);
    while (index < text.length && within(text.charCodeAt(index), 0x20, 0x2f)) {
      index += 1;
    }
    if (index >= text.length) {
      return { end: text.length, sgr: null };
    }
    if (!within(text.charCodeAt(index), 0x40, 0x7e)) {
      return { end: index, sgr: null };
    }
    return { end: index + 1, sgr: text[index] === 'm' ? parameters : null };
  }
  if (kind === ']') {
    for (let index = start + 2; index < text.length; index += 1) {
      if (text[index] === BEL) {
        return { end: index + 1, sgr: null };
      }
      if (text[index] === ESC) {
        return { end: text[index + 1] === '\\' ? index + 2 : index, sgr: null };
      }
    }
    return { end: text.length, sgr: null };
  }
  let index = start + 1;
  while (index < text.length && within(text.charCodeAt(index), 0x20, 0x2f)) {
    index += 1;
  }
  return { end: Math.min(text.length, index + 1), sgr: null };
}

function byte(value: number | undefined): number | null {
  return value !== undefined && Number.isInteger(value) && within(value, 0, 255) ? value : null;
}

function indexed(index: number): Colour {
  if (index < PALETTE) {
    return index;
  }
  if (index >= 232) {
    const grey = 8 + (index - 232) * 10;
    return `rgb(${grey}, ${grey}, ${grey})`;
  }
  const cube = index - PALETTE;
  const red = CUBE[Math.floor(cube / 36)] ?? 0;
  const green = CUBE[Math.floor(cube / 6) % 6] ?? 0;
  const blue = CUBE[cube % 6] ?? 0;
  return `rgb(${red}, ${green}, ${blue})`;
}

function extended(codes: number[], at: number): Extended {
  if (codes[at] === 5) {
    const index = byte(codes[at + 1]);
    return { colour: index === null ? null : indexed(index), used: 2 };
  }
  if (codes[at] === 2) {
    const [red, green, blue] = [byte(codes[at + 1]), byte(codes[at + 2]), byte(codes[at + 3])];
    const colour = red === null || green === null || blue === null ? null : `rgb(${red}, ${green}, ${blue})`;
    return { colour, used: 4 };
  }
  return { colour: null, used: 0 };
}

function codesOf(parameters: string): number[] {
  if (parameters === '') {
    return [0];
  }
  return parameters.split(';').map((part) => (part === '' ? 0 : Number(part)));
}

function styled(look: Look, code: number): Look {
  if (code === 0) {
    return PLAIN;
  }
  if (within(code, 30, 37)) {
    return { ...look, foreground: code - 30 };
  }
  if (within(code, 90, 97)) {
    return { ...look, foreground: code - 82 };
  }
  if (within(code, 40, 47)) {
    return { ...look, background: code - 40 };
  }
  if (within(code, 100, 107)) {
    return { ...look, background: code - 92 };
  }
  switch (code) {
    case 1:
      return { ...look, bold: true };
    case 2:
      return { ...look, dim: true };
    case 3:
      return { ...look, italic: true };
    case 4:
      return { ...look, underline: true };
    case 7:
      return { ...look, inverse: true };
    case 9:
      return { ...look, strike: true };
    case 22:
      return { ...look, bold: false, dim: false };
    case 23:
      return { ...look, italic: false };
    case 24:
      return { ...look, underline: false };
    case 27:
      return { ...look, inverse: false };
    case 29:
      return { ...look, strike: false };
    case 39:
      return { ...look, foreground: null };
    case 49:
      return { ...look, background: null };
    default:
      return look;
  }
}

function graphics(look: Look, parameters: string): Look {
  if (parameters.includes(':') || /[<=>?]/.test(parameters)) {
    return look;
  }
  const codes = codesOf(parameters);
  let next = look;
  for (let index = 0; index < codes.length; index += 1) {
    const code = codes[index] ?? 0;
    if (code === 38 || code === 48) {
      const { colour, used } = extended(codes, index + 1);
      if (colour !== null) {
        next = code === 38 ? { ...next, foreground: colour } : { ...next, background: colour };
      }
      index += used;
    } else {
      next = styled(next, code);
    }
  }
  return next;
}

export function segments(text: string): Segment[] {
  const found: Segment[] = [];
  let look = PLAIN;
  let start = 0;
  let index = 0;
  let pending = '';
  const close = () => {
    if (pending !== '') {
      found.push({ start, text: pending, look });
      pending = '';
    }
  };
  while (index < text.length) {
    if (text[index] !== ESC) {
      if (pending === '') {
        start = index;
      }
      pending += text[index];
      index += 1;
      continue;
    }
    const sequence = escapeAt(text, index);
    if (sequence.sgr !== null) {
      const next = graphics(look, sequence.sgr);
      if (next !== look) {
        close();
        look = next;
      }
    }
    index = sequence.end;
  }
  close();
  return found;
}

export function plain(text: string): string {
  if (!text.includes(ESC)) {
    return text;
  }
  return segments(text)
    .map((segment) => segment.text)
    .join('');
}
