// Bundled Monaco: the core editor API, editor contributions (find, folding, hover, diff
// editor actions…) and the Monarch tokenizers. The TS/JSON/CSS/HTML language services are
// intentionally left out: in a read-only diff view they would only add workers and
// red squiggles for unresolved imports.
import * as monaco from 'monaco-editor/editor/editor.api'
import 'monaco-editor/features/register.all'
import 'monaco-editor/basic-languages/monaco.contribution'
import EditorWorker from 'monaco-editor/editor/editor.worker?worker'
import { loader } from '@monaco-editor/react'
import { SANDY_BEACH_THEME, sandyBeachTheme } from './theme'

self.MonacoEnvironment = {
  getWorker() {
    return new EditorWorker()
  },
}

monaco.editor.defineTheme(SANDY_BEACH_THEME, sandyBeachTheme)
monaco.editor.setTheme(SANDY_BEACH_THEME)

// Monaco binds F7 / Shift+F7 to the accessible diff viewer; we implement our own
// cross-file navigation (see useKeybindings), so drop those defaults.
monaco.editor.addKeybindingRules([
  { keybinding: monaco.KeyCode.F7, command: '-editor.action.accessibleDiffViewer.next' },
  {
    keybinding: monaco.KeyMod.Shift | monaco.KeyCode.F7,
    command: '-editor.action.accessibleDiffViewer.prev',
  },
])

loader.config({ monaco })

// Monaco measures glyph widths once; re-measure after JetBrains Mono finishes loading.
void document.fonts.ready.then(() => monaco.editor.remeasureFonts())

export { monaco }
