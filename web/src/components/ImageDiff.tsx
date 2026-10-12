import { useState } from 'react'
import type { ChangedFile } from '../api'
import { formatBytes, type LoadedFile, type SideInfo } from '../monaco/files'

type ImageState = Extract<LoadedFile, { kind: 'image' }>

function Side({ label, url, info }: { label: string; url: string | null; info: SideInfo }) {
  const [dims, setDims] = useState<string | null>(null)
  return (
    <div className="image-side">
      <div className="image-side-header">
        <span className="image-side-label">{label}</span>
        <span className="muted">
          {url ? [dims, formatBytes(info.bytes)].filter(Boolean).join(' · ') : '(none)'}
        </span>
      </div>
      <div className="image-canvas">
        {url ? (
          <img
            src={url}
            alt={label}
            onLoad={(e) => setDims(`${e.currentTarget.naturalWidth} × ${e.currentTarget.naturalHeight}`)}
          />
        ) : (
          <span className="muted">No file on this side</span>
        )}
      </div>
    </div>
  )
}

export function ImageDiff({ file, state }: { file: ChangedFile; state: ImageState }) {
  return (
    <div className="image-diff" data-testid="image-diff">
      <Side label={`base${file.oldPath && file.oldPath !== file.path ? ` · ${file.oldPath}` : ''}`} url={state.baseUrl} info={state.base} />
      <Side label="head" url={state.headUrl} info={state.head} />
    </div>
  )
}

/** The head image of an unchanged file. */
export function ImageView({ state }: { state: ImageState }) {
  return (
    <div className="image-diff image-single" data-testid="image-view">
      <Side label="head" url={state.headUrl} info={state.head} />
    </div>
  )
}
