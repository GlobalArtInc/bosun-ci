import { render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { HOSTILE_SUMMARIES } from '../test/fixtures/summaries';
import { Markdown } from './markdown';

interface Hacked {
  hacked?: boolean;
}

const FORBIDDEN = [
  'script',
  'iframe',
  'frame',
  'object',
  'embed',
  'style',
  'link',
  'meta',
  'base',
  'form',
  'button',
  'textarea',
  'select',
  'svg',
  'math',
  'video',
  'audio',
  'template',
  'noscript',
];

const ADDRESSES = new Set([
  'href',
  'src',
  'srcset',
  'action',
  'formaction',
  'xlink:href',
  'cite',
  'poster',
  'background',
  'data',
]);

const UNSAFE = /^(javascript|vbscript|data):/i;

function compact(value: string): string {
  return [...value].filter((character) => character.charCodeAt(0) > 0x20).join('');
}

function sources(value: string): string[] {
  return value
    .split(',')
    .map((candidate) => candidate.trim().split(/\s+/)[0] ?? '')
    .filter((address) => address !== '');
}

function problems(container: HTMLElement): string[] {
  const found: string[] = [];
  for (const element of container.querySelectorAll('*')) {
    const tag = element.tagName.toLowerCase();
    if (FORBIDDEN.includes(tag)) {
      found.push(`a <${tag}> element`);
    }
    for (const attribute of element.attributes) {
      const name = attribute.name.toLowerCase();
      if (name.startsWith('on')) {
        found.push(`the handler ${name} on <${tag}>`);
      }
      if (name === 'style') {
        found.push(`a style on <${tag}>`);
      }
      if (ADDRESSES.has(name) && UNSAFE.test(compact(attribute.value))) {
        found.push(`the address ${attribute.value} in ${name} of <${tag}>`);
      }
      if ((name === 'id' || name === 'name') && !attribute.value.startsWith('user-content-')) {
        found.push(`the ${name} ${attribute.value} on <${tag}>`);
      }
    }
    if (tag === 'img' && !(element.getAttribute('src') ?? '').startsWith('https://')) {
      found.push(`an image from ${element.getAttribute('src')}`);
    }
    if (
      tag === 'source' &&
      sources(element.getAttribute('srcset') ?? '').some((address) => !address.startsWith('https://'))
    ) {
      found.push(`a source from ${element.getAttribute('srcset')}`);
    }
  }
  return found;
}

afterEach(() => {
  delete (window as Hacked).hacked;
});

describe('a step summary', () => {
  it.each(HOSTILE_SUMMARIES)('renders none of what $name carries', ({ markdown }) => {
    const { container } = render(<Markdown text={markdown} />);

    expect(problems(container)).toEqual([]);
    expect((window as Hacked).hacked).toBeUndefined();
  });

  it('keeps the markup the target allows', () => {
    const { container } = render(
      <Markdown
        text={[
          '## Results',
          '',
          '| suite | passed |',
          '|-------|--------|',
          '| unit  | **41** |',
          '',
          '- [x] lint',
          '- [ ] deploy',
          '',
          '<details><summary>Coverage</summary>81 <sub>%</sub> <kbd>q</kbd></details>',
          '',
          '![chart](https://example.com/chart.png)',
          '',
          '[the run](https://example.com/run)',
        ].join('\n')}
      />,
    );

    expect(container.querySelector('h2')).toHaveTextContent('Results');
    expect(container.querySelector('table td strong')).toHaveTextContent('41');
    expect(container.querySelectorAll('input[type="checkbox"][disabled]')).toHaveLength(2);
    expect(container.querySelector('details summary')).toHaveTextContent('Coverage');
    expect(container.querySelector('details sub')).toHaveTextContent('%');
    expect(container.querySelector('kbd')).toHaveTextContent('q');
    const image = container.querySelector('img');
    expect(image).toHaveAttribute('src', 'https://example.com/chart.png');
    expect(image).toHaveAttribute('referrerpolicy', 'no-referrer');
    const link = container.querySelector('a');
    expect(link).toHaveAttribute('href', 'https://example.com/run');
    expect(link).toHaveAttribute('rel', 'nofollow noreferrer');
  });

  it('keeps the image of a picture whose sources it drops', () => {
    const { container } = render(
      <Markdown text='<picture><source srcset="http://example.com/a.png"><img src="https://example.com/b.png" alt="b"></picture>' />,
    );

    expect(container.querySelector('source')).toBeNull();
    expect(container.querySelector('img')).toHaveAttribute('src', 'https://example.com/b.png');
  });
});
