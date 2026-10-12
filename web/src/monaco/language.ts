import { monaco } from './setup'

// Monaco's bundled Monarch grammars don't include JSON (it ships with the JSON language
// service, which we don't bundle), so register a small tokenizer for it.
monaco.languages.register({ id: 'json', extensions: ['.json'], aliases: ['JSON'] })
monaco.languages.setMonarchTokensProvider('json', {
  tokenizer: {
    root: [
      [/"(?:[^"\\]|\\.)*"(?=\s*:)/, 'string.key.json'],
      [/"(?:[^"\\]|\\.)*"/, 'string.value.json'],
      [/-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/, 'number'],
      [/\b(?:true|false|null)\b/, 'keyword'],
      [/\/\/.*$/, 'comment'],
      [/\/\*/, 'comment', '@comment'],
      [/[{}[\]]/, 'delimiter.bracket'],
      [/[,:]/, 'delimiter'],
    ],
    comment: [
      [/[^*/]+/, 'comment'],
      [/\*\//, 'comment', '@pop'],
      [/[*/]/, 'comment'],
    ],
  },
})

const BY_EXT: Record<string, string> = {
  ts: 'typescript', tsx: 'typescript', mts: 'typescript', cts: 'typescript',
  js: 'javascript', jsx: 'javascript', mjs: 'javascript', cjs: 'javascript',
  json: 'json', jsonc: 'json', json5: 'json', webmanifest: 'json', har: 'json',
  rs: 'rust', go: 'go', py: 'python', pyi: 'python', rb: 'ruby', php: 'php',
  java: 'java', kt: 'kotlin', kts: 'kotlin', scala: 'scala', sc: 'scala', swift: 'swift',
  dart: 'dart', lua: 'lua', pl: 'perl', pm: 'perl', r: 'r', jl: 'julia',
  ex: 'elixir', exs: 'elixir', clj: 'clojure', cljs: 'clojure', cljc: 'clojure', edn: 'clojure',
  fs: 'fsharp', fsi: 'fsharp', fsx: 'fsharp', cs: 'csharp', csx: 'csharp', vb: 'vb',
  c: 'c', h: 'c', cc: 'cpp', cpp: 'cpp', cxx: 'cpp', hpp: 'cpp', hh: 'cpp', hxx: 'cpp',
  ino: 'cpp', cu: 'cpp', cuh: 'cpp', m: 'objective-c', mm: 'objective-c',
  html: 'html', htm: 'html', xhtml: 'html', vue: 'html', svelte: 'html',
  xml: 'xml', svg: 'xml', xsd: 'xml', xsl: 'xml', plist: 'xml', csproj: 'xml', props: 'xml',
  css: 'css', scss: 'scss', sass: 'scss', less: 'less',
  md: 'markdown', markdown: 'markdown', mdx: 'mdx', rst: 'restructuredtext',
  yml: 'yaml', yaml: 'yaml', toml: 'ini', ini: 'ini', cfg: 'ini', conf: 'ini', properties: 'ini',
  editorconfig: 'ini', gitconfig: 'ini',
  sh: 'shell', bash: 'shell', zsh: 'shell', ksh: 'shell', fish: 'shell', envrc: 'shell',
  ps1: 'powershell', psm1: 'powershell', psd1: 'powershell', bat: 'bat', cmd: 'bat',
  sql: 'sql', psql: 'pgsql', graphql: 'graphql', gql: 'graphql', proto: 'proto',
  dockerfile: 'dockerfile', tf: 'hcl', tfvars: 'hcl', hcl: 'hcl',
  hbs: 'handlebars', handlebars: 'handlebars', twig: 'twig', liquid: 'liquid', pug: 'pug',
  cshtml: 'razor', sol: 'sol', sv: 'systemverilog', svh: 'systemverilog', v: 'verilog',
  vh: 'verilog', wgsl: 'wgsl', tcl: 'tcl', scm: 'scheme', ss: 'scheme', rkt: 'scheme',
  pas: 'pascal', coffee: 'coffeescript', cypher: 'cypher', tsp: 'typespec',
}

const BY_NAME: Record<string, string> = {
  dockerfile: 'dockerfile',
  containerfile: 'dockerfile',
  gemfile: 'ruby',
  rakefile: 'ruby',
  podfile: 'ruby',
  vagrantfile: 'ruby',
  brewfile: 'ruby',
  justfile: 'shell',
  '.bashrc': 'shell',
  '.zshrc': 'shell',
  '.profile': 'shell',
  '.gitattributes': 'ini',
  '.gitmodules': 'ini',
  '.npmrc': 'ini',
  'go.mod': 'go',
  'cargo.lock': 'ini',
  pipfile: 'ini',
}

/** Monaco language id for a repository path, `plaintext` when unknown. */
export function languageForPath(path: string): string {
  const name = path.slice(path.lastIndexOf('/') + 1).toLowerCase()
  const byName = BY_NAME[name]
  if (byName) return byName
  if (name.startsWith('dockerfile.') || name.endsWith('.dockerfile')) return 'dockerfile'
  const dot = name.lastIndexOf('.')
  if (dot < 0) return 'plaintext'
  return BY_EXT[name.slice(dot + 1)] ?? 'plaintext'
}
