import ReactMarkdown, {
  type AllowElement,
  type Components,
  defaultUrlTransform,
  type UrlTransform,
} from 'react-markdown';
import rehypeRaw from 'rehype-raw';
import rehypeSanitize, { defaultSchema } from 'rehype-sanitize';
import remarkGfm from 'remark-gfm';

interface MarkdownProps {
  text: string;
}

const HTTPS = 'https:';

const SCHEMA = {
  ...defaultSchema,
  protocols: { ...defaultSchema.protocols, src: ['https'] },
};

const LOADED = new Set(['img', 'source']);

function isHttps(address: string): boolean {
  try {
    return new URL(address).protocol === HTTPS;
  } catch {
    return false;
  }
}

function candidates(value: unknown): string[] {
  const listed = Array.isArray(value) ? value.map(String) : typeof value === 'string' ? value.split(',') : [];
  return listed
    .map((candidate) => candidate.trim().split(/\s+/)[0] ?? '')
    .filter((address) => address !== '');
}

const safeUrl: UrlTransform = (address, key) => {
  if (key === 'src') {
    return isHttps(address) ? address : '';
  }
  return defaultUrlTransform(address);
};

const loadsOnlyHttps: AllowElement = (element) => {
  if (!LOADED.has(element.tagName)) {
    return true;
  }
  const { src, srcSet } = element.properties;
  const sources = [...(src === undefined || src === null ? [] : [String(src)]), ...candidates(srcSet)];
  if (element.tagName === 'img' && sources.length === 0) {
    return false;
  }
  return sources.every(isHttps);
};

const COMPONENTS: Components = {
  a: ({ node, ...link }) => <a {...link} rel="nofollow noreferrer" />,
  img: ({ node, ...image }) => (
    <img {...image} alt={image.alt ?? ''} referrerPolicy="no-referrer" loading="lazy" />
  ),
};

export function Markdown({ text }: MarkdownProps) {
  return (
    <div className="markdown">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[rehypeRaw, [rehypeSanitize, SCHEMA]]}
        urlTransform={safeUrl}
        allowElement={loadsOnlyHttps}
        components={COMPONENTS}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
}
