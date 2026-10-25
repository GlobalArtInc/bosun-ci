export interface HostileSummary {
  name: string;
  markdown: string;
}

export const HOSTILE_SUMMARIES: HostileSummary[] = [
  { name: 'a script element', markdown: '<script>window.hacked = true</script>' },
  {
    name: 'a script split by markup',
    markdown: '<scr<script>ipt>window.hacked = true</scr</script>ipt>',
  },
  { name: 'a script inside svg', markdown: '<svg><script>window.hacked = true</script></svg>' },
  { name: 'a load handler of svg', markdown: '<svg onload="window.hacked = true"><circle r="4"/></svg>' },
  {
    name: 'an error handler on an image',
    markdown: '<img src="https://example.com/x.png" onerror="window.hacked = true">',
  },
  {
    name: 'an event handler deep in nested markup',
    markdown:
      '<div><blockquote><p><b onmouseover="window.hacked = true">bold <i onclick="window.hacked = true">and</i></b></p></blockquote></div>',
  },
  {
    name: 'a toggle handler on details',
    markdown: '<details open ontoggle="window.hacked = true"><summary>more</summary>text</details>',
  },
  { name: 'a focus handler on an input', markdown: '<input autofocus onfocus="window.hacked = true">' },
  { name: 'a javascript link in Markdown', markdown: '[click](javascript:window.hacked=true)' },
  { name: 'a javascript link in mixed case', markdown: '[click](JaVaScRiPt:window.hacked=true)' },
  { name: 'a javascript link in HTML', markdown: '<a href="javascript:window.hacked=true">click</a>' },
  {
    name: 'a javascript link written with entities',
    markdown: '<a href="&#106;&#x61;vascript&colon;window.hacked=true">click</a>',
  },
  {
    name: 'a javascript link split by white space',
    markdown: '<a href=" java\tscript:window.hacked=true">click</a>',
  },
  { name: 'a reference link to javascript', markdown: '[click][x]\n\n[x]: javascript:window.hacked=true' },
  { name: 'an autolink to javascript', markdown: '<javascript:window.hacked=true>' },
  { name: 'a vbscript link', markdown: '<a href="vbscript:msgbox(1)">click</a>' },
  {
    name: 'a data link',
    markdown: '<a href="data:text/html,%3Cscript%3Ewindow.hacked=true%3C/script%3E">click</a>',
  },
  { name: 'an http image in Markdown', markdown: '![tracker](http://example.com/pixel.png)' },
  { name: 'an http image in HTML', markdown: '<img src="http://example.com/pixel.png" alt="tracker">' },
  { name: 'an image without a scheme', markdown: '<img src="//example.com/pixel.png">' },
  { name: 'a relative image', markdown: '![me](/api/v1/me)' },
  { name: 'a data image', markdown: '<img src="data:image/png;base64,iVBORw0KGgo=">' },
  {
    name: 'an http source in a picture',
    markdown:
      '<picture><source srcset="http://example.com/a.png 1x, https://example.com/b.png 2x"><img src="https://example.com/c.png"></picture>',
  },
  {
    name: 'an inline frame',
    markdown: '<iframe src="https://example.com" srcdoc="<script>x</script>"></iframe>',
  },
  {
    name: 'an object and an embed',
    markdown:
      '<object data="javascript:window.hacked=true"></object><embed src="javascript:window.hacked=true">',
  },
  {
    name: 'a style element and a style attribute',
    markdown: '<style>body { display: none }</style><div style="position: fixed; inset: 0">cover</div>',
  },
  {
    name: 'a form with a password field',
    markdown:
      '<form action="https://example.com/steal"><input name="password" type="password"><button formaction="https://example.com">go</button></form>',
  },
  {
    name: 'a refresh and a base address',
    markdown:
      '<meta http-equiv="refresh" content="0;url=https://example.com"><base href="https://example.com/">',
  },
  { name: 'a stylesheet link', markdown: '<link rel="stylesheet" href="https://example.com/x.css">' },
  {
    name: 'math with a javascript link',
    markdown: '<math><mtext><a xlink:href="javascript:window.hacked=true">x</a></mtext></math>',
  },
  {
    name: 'a template and a noscript',
    markdown:
      '<template><script>window.hacked=true</script></template><noscript><img src=x onerror="window.hacked=true"></noscript>',
  },
  {
    name: 'a background of a table cell',
    markdown: '<table><tr><td background="javascript:window.hacked=true">x</td></tr></table>',
  },
  { name: 'a comment that hides a script', markdown: '<!--><script>window.hacked=true</script>-->' },
  {
    name: 'a Markdown table that carries markup',
    markdown:
      '| a | b |\n|---|---|\n| <img src=x onerror="window.hacked=true"> | <script>window.hacked=true</script> |',
  },
  {
    name: 'ids and names that clobber the document',
    markdown:
      '<a id="location" name="cookie">x</a><img name="getElementById" src="https://example.com/x.png">',
  },
  {
    name: 'a video with an http poster',
    markdown: '<video poster="http://example.com/x.png" src="https://example.com/x.mp4" autoplay></video>',
  },
];
