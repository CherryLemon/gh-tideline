// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import { colorizedRuns } from './colorized'

describe('colorizedRuns', () => {
  it('reads the runs of text and their token classes from Monaco colorized line', () => {
    const html = '<span><span class="mtk4">export</span><span class="mtk1">&nbsp;</span><span class="mtk7 mtki">class</span></span>'
    expect(colorizedRuns(html)).toEqual([
      { text: 'export', className: 'mtk4' },
      { text: ' ', className: 'mtk1' },
      { text: 'class', className: 'mtk7 mtki' },
    ])
  })

  it('turns no-break spaces back into spaces and decodes entities', () => {
    const html = '<span><span class="mtk1">a&#160;&#160;&lt;b&gt;&nbsp;&amp;</span></span>'
    expect(colorizedRuns(html)).toEqual([{ text: 'a  <b> &', className: 'mtk1' }])
  })

  it('keeps repository text as text: a tag in the line comes out as characters', () => {
    const html = '<span><span class="mtk1">render(html&#160;=&#160;\'&lt;img&#160;src=x&#160;onerror=alert(1)&gt;\'):&#160;string</span></span>'
    const runs = colorizedRuns(html)
    expect(runs).toEqual([{ text: "render(html = '<img src=x onerror=alert(1)>'): string", className: 'mtk1' }])
  })

  it('drops everything but text and token classes, even from markup that is not Monaco', () => {
    const html =
      '<span><span class="evil" onclick="alert(1)" style="x">a</span><span class="mtk1 evil">b</span><img src=x onerror=alert(1)>' +
      '<script>alert(1)</script><span class="mtk2"><b>c</b></span><span class="mtk3">d</span></span>'
    const runs = colorizedRuns(html)
    expect(runs).toEqual([
      { text: 'a', className: '' },
      { text: 'b', className: '' },
      { text: 'd', className: 'mtk3' },
    ])
  })

  it('accepts only the class names Monaco writes for tokens', () => {
    const classes = ['mtk1', 'mtk12', 'mtki', 'mtkb', 'mtku', 'mtks', 'mtk3 mtki mtkb']
    for (const c of classes) expect(colorizedRuns(`<span><span class="${c}">x</span></span>`)).toEqual([{ text: 'x', className: c }])
    for (const c of ['mtk', 'mtkx', 'mtk1x', 'mtk1  mtk2', 'mtk1 foo', 'foo', 'MTK1', ' mtk1']) {
      expect(colorizedRuns(`<span><span class="${c}">x</span></span>`)).toEqual([{ text: 'x', className: '' }])
    }
  })

  it('returns no runs for an empty line', () => {
    expect(colorizedRuns('<span></span>')).toEqual([])
    expect(colorizedRuns('')).toEqual([])
  })
})
