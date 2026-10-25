import { describe, expect, it } from 'vitest';
import { plain, segments } from './ansi';

const ESC = '\u001b';

function looks(text: string) {
  return segments(text).map((segment) => [segment.text, segment.look.foreground, segment.look.background]);
}

describe('the ANSI sequences of a log line', () => {
  it('leave a line without them as one plain segment', () => {
    const [only, ...rest] = segments('compiled bosun-types');
    expect(rest).toEqual([]);
    expect(only?.text).toBe('compiled bosun-types');
    expect(only?.look.foreground).toBeNull();
  });

  it('colour the text until a reset, in the sixteen colours and their bright forms', () => {
    expect(looks(`${ESC}[32mok${ESC}[0m compiled ${ESC}[91;44mfailed${ESC}[39;49m done`)).toEqual([
      ['ok', 2, null],
      [' compiled ', null, null],
      ['failed', 9, 4],
      [' done', null, null],
    ]);
  });

  it('read the 256 colours and the colours given by their red, green and blue', () => {
    expect(looks(`${ESC}[38;5;208mamber${ESC}[48;5;244mgrey${ESC}[38;5;3m${ESC}[48;2;1;2;3mdeep`)).toEqual([
      ['amber', 'rgb(255, 135, 0)', null],
      ['grey', 'rgb(255, 135, 0)', 'rgb(128, 128, 128)'],
      ['deep', 3, 'rgb(1, 2, 3)'],
    ]);
  });

  it('ignore a colour out of range and parameters that are not numbers', () => {
    expect(looks(`${ESC}[38;2;300;0;0mwide${ESC}[38;5;256mmore${ESC}[?25lhidden${ESC}[4:3mcurly`)).toEqual([
      ['widemorehiddencurly', null, null],
    ]);
  });

  it('set and clear the weight, the slant, the lines and the inversion', () => {
    const [bold, inverse, normal] = segments(
      `${ESC}[1;3;4;9mbold${ESC}[7minverse${ESC}[22;23;24;27;29mnormal`,
    );
    expect(bold?.look).toMatchObject({
      bold: true,
      italic: true,
      underline: true,
      strike: true,
      inverse: false,
    });
    expect(inverse?.look).toMatchObject({ bold: true, inverse: true });
    expect(normal?.look).toMatchObject({
      bold: false,
      italic: false,
      underline: false,
      strike: false,
      inverse: false,
    });
  });

  it('drop the sequences that move the cursor, set a title or link, and a sequence left unfinished', () => {
    expect(plain(`a${ESC}[2Kb${ESC}[1Ac${ESC}(Bd`)).toBe('abcd');
    expect(plain(`${ESC}]8;;javascript:alert(1)${ESC}\\click${ESC}]8;;${ESC}\\`)).toBe('click');
    expect(plain(`${ESC}]0;a title\u0007text`)).toBe('text');
    expect(plain(`text${ESC}]8;;https://example.com`)).toBe('text');
    expect(plain(`text${ESC}[31`)).toBe('text');
    expect(plain(`text${ESC}`)).toBe('text');
  });
});
