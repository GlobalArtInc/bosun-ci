import { type CSSProperties, Fragment } from 'react';
import { type Colour, type Look, segments } from './ansi';

interface AnsiTextProps {
  text: string;
}

interface Paint {
  classes: string[];
  style: CSSProperties;
}

type Layer = 'fg' | 'bg';

function paint(into: Paint, colour: Colour | null, layer: Layer, swapped: boolean) {
  if (colour === null) {
    if (swapped) {
      into.classes.push(`ansi-${layer}-inverse`);
    }
    return;
  }
  if (typeof colour === 'number') {
    into.classes.push(`ansi-${layer}-${colour}`);
  } else if (layer === 'fg') {
    into.style.color = colour;
  } else {
    into.style.backgroundColor = colour;
  }
}

function paintOf(look: Look): Paint {
  const into: Paint = { classes: [], style: {} };
  if (look.inverse) {
    paint(into, look.background, 'fg', true);
    paint(into, look.foreground, 'bg', true);
  } else {
    paint(into, look.foreground, 'fg', false);
    paint(into, look.background, 'bg', false);
  }
  if (look.bold) {
    into.classes.push('ansi-bold');
  }
  if (look.dim) {
    into.classes.push('ansi-dim');
  }
  if (look.italic) {
    into.classes.push('ansi-italic');
  }
  if (look.underline || look.strike) {
    into.classes.push(
      look.underline && look.strike
        ? 'ansi-underline-strike'
        : look.underline
          ? 'ansi-underline'
          : 'ansi-strike',
    );
  }
  return into;
}

export function AnsiText({ text }: AnsiTextProps) {
  return (
    <>
      {segments(text).map((segment) => {
        const { classes, style } = paintOf(segment.look);
        if (classes.length === 0 && Object.keys(style).length === 0) {
          return <Fragment key={segment.start}>{segment.text}</Fragment>;
        }
        return (
          <span key={segment.start} className={classes.join(' ')} style={style}>
            {segment.text}
          </span>
        );
      })}
    </>
  );
}
