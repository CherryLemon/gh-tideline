# Tideline Design Document

- Status: draft, current through M3 (v0.4.0)
- Date: 2026-10-15

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
| **M1 review context** | Viewed state synced both ways with GitHub; the full project tree |
| **M2 review threads and writing** | Existing review threads in the diff (moved here from M1); comments on lines, line ranges and whole files, as a pending review or a single comment; replies, resolve / unresolve, editing and deleting your own comments; submitting a review (Comment / Approve / Request changes) with a summary; Markdown preview |
| **M3 code navigation** | tree-sitter symbol index, built on the first jump; go to definition (Ctrl+B / Ctrl+click); usages of a name from the name of a definition (the same keys, no hotkey of their own); sticky header showing the enclosing functions and classes; back / forward through visited locations (Ctrl+Alt+← / Ctrl+Alt+→), as in JetBrains |
| **Later** | View a single commit or a commit range; "what changed since the last review" (handles rebases). Both were planned for M1 |

Each milestone ships as a minor release: M0 is v0.1.0, M1 v0.2.0, M2 v0.3.0 and M3 v0.4.0.

### Out of scope (through M3)

- No writes beyond reviews: suggested changes (suggestion blocks), requesting reviewers, conversation comments outside a review, reactions, merging. Do these on the GitHub website.
- No JetBrains-style curved connector bands. Monaco aligns the two sides by inserting blank lines. We accept that for this phase.
- No LSP, and no three-way merge view. Code navigation matches names; it does not resolve types.
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
| o/r #123  Add foo support   main <- feat/foo   [Split|Unified]  [Review (2)] |
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
- A toggle at the top switches to the full project tree: every file at the head, with the changed files (and deleted ones, at their old path) in place and marked by status.
  Only folders leading to changes start open. Clicking an unchanged file shows its head content read-only.

**Status bar.** On the left, the position (Change i/n and File i/n); next to it "Looking up…" while a lookup of a name waits (M3, see 5.8). On the right, the shortcuts. Those that do not fit the width are left out whole, the last first, so that none is cut in half.

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
| c (M2) | Comment on the current line, or on the selected lines. A selection that ends at column 1 leaves out its last line |
| Ctrl+Enter (M2) | In a comment box: its main button (Start a review, Add review comment, Update comment). In the review dialog: Submit review |
| Esc (M2, M3) | In a comment box: close it, asking first if it holds unsent changes. In the review dialog: close it; the summary is kept. In the dialog that shows the index being built (M3): cancel the build and drop the jump. In a list of declarations or usages (M3): close it and give the focus back to the editor |
| Ctrl+B / Ctrl+click (M3) | Go to definition of the name at the caret / under the pointer (Cmd instead of Ctrl on macOS). On the name of a definition itself it lists the places that use the name instead, and several definitions are listed to choose from (see 5.8). There is no key of its own for usages |
| Ctrl+Alt+← / Ctrl+Alt+→ (M3) | Back / forward through visited locations, like JetBrains' Navigate Back / Forward |

**Notes:**
- Shortcuts that the browser itself reserves can't be intercepted, such as Ctrl+T, Ctrl+W, Ctrl+N and Ctrl+Shift+N.
  So JetBrains' Ctrl+Shift+N (find file by name) becomes double Shift or Ctrl+P.
- **Back and forward (M3):** Ctrl+Alt+← / Ctrl+Alt+→ go back and forward through the locations visited: files opened (from the tree, the file finder or Alt+↓ / Alt+↑), F7 jumps into another file, jumps to a definition and usages picked from a list.
  Each of these is a browser history entry (`pushState`). It carries the place in `history.state`, so the browser's own Back / Forward (Alt+← / Alt+→, the mouse buttons) work too, and return to the caret.
  The caret is recorded when the user leaves the entry, and again once it has rested for a second, so that a reload also returns to it.
  One step per key press: holding the keys does not repeat. Back stops at the first entry the app made and never leaves the app; only the browser's own button does.
  Some Linux desktops take Ctrl+Alt+← / → to switch workspaces (GNOME does by default, and KDE may), so the browser's keys remain the fallback there. There is no alternative for macOS yet.
- **In comment boxes, keys are text:** the other shortcuts (`v`, `c`, double Shift, F7, Alt+↓, Ctrl+B, Ctrl+Alt+← / →) are off there, on the box's buttons and tabs too. Ctrl+P still opens the file finder, and the box keeps its draft. While a dialog is open (the file finder, the review dialog, the dialog of an index being built, a list of declarations or usages), none of them work.

## 4. Architecture

### 4.1 Overview

```
 gh tideline <pr>  ──start / reuse──▶  local server (Rust, 127.0.0.1:PORT)
                                ├─ git layer ──── runs git commands ──▶ cache repo (bare + partial + shallow)
                                ├─ GitHub layer ─ GraphQL / REST ──▶ api.github.com / GHE
                                ├─ index layer (M3) ─ tree-sitter, our own tags queries ──▶ SQLite
                                └─ HTTP: /api/*, static assets (embedded in the binary)
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
| Index (M3) | tree-sitter + per-language grammar crates, with our own tags queries and our own runner for them (not the `tree-sitter-tags` crate); stored with rusqlite | Tags queries (the `tags.scm` format) are what GitHub's "search-based code navigation" uses. Ours start from the queries the grammars ship and add what they lack (see 5.8). The `tree-sitter-tags` crate can neither be cancelled nor given a time limit, and some files (deeply nested code, long runs of comments) take it tens of seconds or more |
| Frontend | Vite + React + TS | |
| Editor | monaco-editor (bundled into the frontend, not loaded from a CDN) + @monaco-editor/react | Built-in side-by-side / unified views, collapsing of unchanged regions, word-level highlighting and view zones. Ctrl+click and Ctrl+B are handled by the app itself, not through a Monaco definition provider. A `DocumentSymbolProvider` on the `dr` scheme gives the breadcrumbs of collapsed regions |
| Highlighting | Monaco's built-in Monarch tokenizers | Built into Monaco. The rows of the sticky header are colored through `monaco.editor.colorize` |
| Lists | @tanstack/react-virtual | The file tree must handle 3000 files |
| Requests | `fetch`, in the wrappers of `web/src/api.ts`, with React state | Small purpose-built caches where needed: the Monaco models and the symbols of files are kept in LRUs |

**Required git version: at least 2.40**, because we need `merge-tree --merge-base` and `check-attr --source`.
This machine has 2.43. The program checks the version at startup. If it is too old, it exits with an error and a hint.

### 4.3 Rust modules

Start with a single crate; each item below is one module. Split it into a workspace later if it grows.

```
src/
  main.rs        CLI: parse input, start / reuse the server, open the browser
  server/        axum routes, security middleware, static assets
  git/           cache repo management, fetch, diff, cat-file process pool, one lock per repo
  github/        token lookup, GraphQL / REST client, ETag cache
  pr/            ties git and GitHub together: open a PR, file list, review threads and review writing
  index/  (M3)   symbol extraction with tree-sitter tags queries, SQLite, index build jobs, definition and usage lookup
  config.rs      config file ~/.config/tideline/config.toml
web/             frontend project; rust-embed embeds its build output
```

### 4.4 Cache directory

```
$XDG_CACHE_HOME/tideline/
  server.json                          running server: pid, port, version
  repos/<host>/<owner>/<repo>.git      bare repo, partial clone + shallow
  state.sqlite                         local state: recently opened PRs, the head last viewed in this tool for each PR
  index.sqlite             (M3)        symbol index, keyed by blob OID, plus the file listing of each indexed commit
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

- **One load, nothing pushed:** steps 1–8 run as one load, and `GET /pulls/{n}` and `/files` answer from its result: the file tree appears when it is done. A result is reused for 30 seconds, and concurrent requests share one load.
  Line counts come from step 7 (`numstat`); GitHub's counts from step 2 stand in for a file that `numstat` has none for.
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
| Larger than 5 MB, or more than 50,000 lines | Don't load automatically; show a "Load file" button |
| Longest line over 10,000 characters (minified files) | Turn off highlighting and word-level diff |

Monaco also has two limits of its own that must line up with the table above: `maxComputationTime` (default 5000 ms) and `maxFileSize` (default 50 MB).
When the diff computation times out, the result looks bad, so files above the thresholds in the table never go to Monaco for diffing.

**Line endings and encoding:**
- Monaco normalizes each model to a single line ending. If the only change is CRLF to LF, Monaco shows "no changes", while numstat counts every line as changed.
- So the server detects line-ending changes, BOMs, and UTF-16 or other non-UTF-8 encodings. When it finds one, the UI shows a notice at the top of the file.

**Binary files:**
- A file that numstat shows as `-	-` is binary.
- Images: the frontend `fetch`es the bytes, creates a `blob:` URL and compares the two sides with `<img>`. It doesn't use the blob endpoint URL directly as the img src. Section 8 explains why.
- Other binary files: show only "Binary file, size a → b".

**Ignoring whitespace:** Monaco only supports `ignoreTrimWhitespace` (ignore leading/trailing whitespace), so M0 offers only two settings: "don't ignore" and "ignore leading/trailing whitespace".
"Ignore all whitespace" would require our own diff computation. That is left for later.

**Line counts in the file tree** come from git numstat and may differ slightly from what the Monaco view shows. This is acceptable.

### 5.3 Viewed state sync (M1)

- **Read:** `pullRequest.files(first:100, after:$cursor) { nodes { path viewerViewedState } }`. There are three states: VIEWED, UNVIEWED and DISMISSED.
- **Write:** `markFileAsViewed(input:{pullRequestId, path})`; to undo, use `unmarkFileAsViewed`. Renamed files use the new path.
- **The frontend updates the UI first, then sends the request.** If the request fails, it rolls back and shows a toast.
- **Record the local head:** after the user marks every checkable file (generated files excluded) as viewed in this tool, store the current head SHA in `state.sqlite`. 5.7 uses it as the fallback baseline. Recording the head after marking just one file would be wrong.

### 5.4 Review threads (M2)

- **Query:** `reviewThreads(first:100)`: each thread's `path subjectType diffSide startDiffSide line startLine originalLine originalStartLine isResolved isOutdated isCollapsed` and the viewer's permissions,
  with `comments(first:100) { author bodyHTML state pullRequestReview{id} replyTo ... }`. Both levels are paged; a thread with more comments gets a follow-up `node(id)` query.
  The same read returns the viewer's pending review (`reviews(states:[PENDING], first:5)`, keeping the one with `viewerDidAuthor`) and their latest submitted review (`viewerLatestReview`).
- **Normalizing (S14):** single-line threads come back with `startLine == line` and `startDiffSide: null` (no range). FILE threads come back with `line: 1, diffSide: RIGHT` (ignored).
  Right after a pending review is deleted, a thread can still be listed with zero comments: those are dropped.
- **Where they show:**
  - Threads that are not outdated: a card in a view zone below their line, on their side (LEFT is the old file, RIGHT is the new file).
    In unified mode the original side takes no zones (S8), so a LEFT thread goes below its line's counterpart in the new file; on a removed line, after the lines that change adds (or above the removed lines when it adds none).
  - Lines inside regions that `hideUnchangedRegions` collapsed are expanded first, so their cards show.
  - File-level threads (`subjectType: FILE`, `line` null), outdated threads (with their `diffHunk`) and threads that can't be placed (with the reason) go in a panel above the editor.
  - Resolved threads stay in place, collapsed. The viewer's pending comments carry a "Pending" chip.
  - When the threads were read at another head than the one shown, line threads go to the panel too, with a Refresh button.
  - The file tree shows each file's number of unresolved threads in the accent color, or its thread count in grey once all are resolved.
- **Line numbers can be used directly.** GitHub's `line` is the line number in the whole file. It matches our full-file models, so no conversion is needed.
- **Security:** `bodyHTML` is already rendered by GitHub, but we still sanitize it (see section 8).

### 5.5 Writing reviews (M2)

**The model is github.com's.** Comments wait in the viewer's pending review ("Start a review", then "Add review comment") until the review is submitted. "Add single comment" posts one at once.
GitHub allows one pending review per user and PR, and no single comment while it exists (S13), so a box only offers what GitHub takes.
Drafts live on GitHub, not in Tideline: a review started here can be finished on github.com, and the other way round.

**Comment boxes:**
- On lines: the "+" in the gutter (drag it for a range), or `c` on the current line or selection. Ranges stay on one side. Old lines take comments in split view only.
  GitHub takes lines within 3 lines of a change (S12). Monaco's diff can differ from git's, so GitHub has the final word, and its refusal is shown.
- On a whole file: "Comment on file" in the file header, for every changed file whatever its kind (binary, renamed, deleted).
- In the thread cards: reply, resolve / unresolve, and edit or delete your own comments (delete asks first).
- Write / Preview tabs. GitHub renders the preview (REST `POST /markdown`, `gfm` with the repo as context, so `#123` and `@user` link as they will), and it is sanitized like a comment body.
  Renders are cached per text; a failed one isn't, so asking again retries.
- The text of open boxes is kept in `sessionStorage` until it is sent, so it survives a reload or a file switch.
  Each box remembers the head it was started on; a line box from an older head asks to confirm its lines before sending.
- Cards and boxes live in view zones, each in an overlay widget that keeps its React state and focus across diff recomputes.
  Key, input, composition and clipboard events stop at the card (S16), so typing never reaches Monaco.

**Writes, on the server:**
- Creating a thread or a reply, submitting and discarding start with a fresh read of the review state: the PR id, its head, the viewer's pending review. Nothing is cached.
  Deleting reads it after the call, to return what is left; resolving and editing are single calls.
- **Head checks (409 `stale_head`):** the comment, reply or review must name the head the page shows
  - always for a comment on lines: GitHub anchors new threads to its current head (S13), so line numbers from an older head would land on the wrong lines;
  - for any other comment or reply, unless it goes into an existing pending review (a single comment is a review of its own);
  - for submitting, so a review never approves commits the reviewer hasn't seen.

  Resolving, editing, deleting and discarding don't check the head.
- 409 `pending_review_exists`: a single comment or reply while a pending review exists. 409 `review_changed`: the pending review on GitHub isn't the one the page shows.
- 422 `rejected`: GitHub refused the write (`UNPROCESSABLE` or `FORBIDDEN`, or a `null` thread for a line outside the diff, S12). Its message is shown as text.
- Review writes to one PR (creating, replying, submitting, discarding, deleting) run one at a time, from reading the state to the last call,
  so two writes can't both start a review, or add to one the other is submitting.
- Those writes also run to their end even if the browser goes away midway (a reload while sending).
- **Creates are never retried automatically.** After an answer that may hide a landed write (network error, 502, 504), the client reads the threads again
  and looks for its comment (written by the viewer, same text) or its review before it offers to send again.

**Submitting (the topbar's "Review (N)" opens a dialog):**
- A summary (kept with the drafts) and Comment, Approve or Request changes. GitHub refuses the last two on your own PR, so there the dialog offers only Comment.
- A review with neither comments nor a summary needs a summary, unless it approves.
- The dialog warns when the pending review was started on an older commit (its comments stay on that version's lines), and discards the pending review after asking.
- A submit refused for new commits (409 `stale_head`) says so; refreshing loads them and closes the dialog, which says why: look at them before submitting. The summary is kept.

### 5.6 Viewing by commit or range (later)

- **Getting the commit list:** GraphQL `commits(first:100) { nodes { commit { oid messageHeadline authoredDate author{name} parents(first:2){nodes{oid}} } } }`.
  GitHub's REST list of PR commits returns at most 250 entries. Whether GraphQL `commits` has the same limit must be tested (see S7).
- **Fetching:** shallow-fetch all of the PR's commits and their parents by SHA in one go (depth=1, blob:none).
  No history walk is needed, because the diff of any range is just a comparison between two trees.
- **Selector:**
  - Single commit: parent → that commit. For a merge commit, use the first parent.
  - Range [A..B]: A's parent → B.
  - The state lives in the URL: `?from=<sha>&to=<sha>`.

### 5.7 What changed since the last review (later)

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
  If the cache doesn't have it either, show "The last reviewed version is no longer available".
  Don't fall back to REST `git/trees` / `git/blobs`: they read the same object store, so when git can't fetch an object, they return 404 too.

**3. Compute the diff to show:**

| Case | Approach |
|---|---|
| `M_old == M_new` (no rebase) | Compare H_old → H_new directly |
| A rebase happened | First replay the old PR onto the new base: `T = git merge-tree --write-tree --merge-base=M_old M_new H_old`. If there is no conflict, show T → H_new. This leaves only the author's own changes, with no changes from the base branch mixed in |
| The replay has conflicts | Fall back to H_old → H_new, and show "Includes changes from the base branch" at the top |

**4. UI:** add an entry "Since your last review (abc1234)" to the commit selector.

### 5.8 Symbol index and code navigation (M3)

**What a lookup is.** Names are matched as text, not resolved by type. The index holds the definitions, and the references (calls, uses of a type, and names used as values, such as a function passed to a call), that tree-sitter queries find in the source files of a commit. A lookup returns those with the same name in files of the same language family, nearest to the file of the click first. Wherever usages are listed, the UI says "matched by name".

**When the index is built.** On the first jump, not when a pull request is opened. There is no setting for it, and nothing is pushed to the page.
- A lookup asks the server for definitions. When the index of that commit is not complete, the answer is 409 `index_missing`: the page starts a build (`POST .../index`), polls it (`GET`, every 300 ms) and asks again once it is ready. A commit that is indexed answers at once.
- A modal dialog shows the phase (listing, downloading, parsing, saving) and a count, `done / total`. It appears only after the page has waited 300 ms for the build, so an index that is nearly there flashes nothing. The page behind it is inert.
- Cancel (the button, or Esc) sends `DELETE` and drops the jump. Like any other end of the lookup (see below), it ends the wait at once, even if the request that starts the build is still unanswered; that request goes on, and is not followed or sent again. The `DELETE` goes out once that request is answered or has failed, or 3 seconds after it was sent, so that it does not overtake the start it is for. The next build of that commit waits for the server to answer the `DELETE`, so that the cancel does not reach the server after the new build has started, and take it down. It waits 3 seconds at most, counted from when the `DELETE` is sent; with a start and a `DELETE` that the server never answers, that makes 6 seconds. A build that failed shows its message, with Close and Retry, and so does a call that keeps failing, the start of the build or a check of its status. A call that fails for a moment (no connection, a 408, a 429 or a 5xx) is tried again first, after 0.5, 1 and 2 seconds. So that a request the server never answers does not hold a connection of the page for good, the browser gives up on a start after 30 seconds, which counts as no connection, and on a `DELETE` after 10 seconds, which is dropped without a word. A refusal (any other 4xx) is not tried again, and neither is the 503 `index_unavailable` (the server cannot use its index, and a moment does not change that): both end the lookup with a toast, which offers Refresh when the refusal is a 409 `stale_head`. After Retry the dialog is back at the progress, with its Cancel, at once, without waiting for the server to answer the new start. A build that someone else cancelled, say from another window, ends the lookup with a toast. A job the server no longer knows (it restarted) is started again, twice at most while the dialog waits for it, and an index that went missing by the time of the lookup is built again, 3 times at most.
- A lookup from the old side of a diff (`side=base`) indexes the merge base, on demand. It shares most blobs with the head, so what is left to do is small.
- One lookup is under way at a time. Starting another, moving the caret, leaving the history entry, a new head, or opening something else over the page (the file finder, the review dialog, a comment box), or going to another text field, ends it. The build it was waiting for goes on; only Cancel stops that. An answer that arrives once the user is at a text field or in a dialog of the page is dropped, so that it does not take the focus from them. While a lookup waits and no dialog shows, the status bar says "Looking up…" (after 300 ms, as the dialog is).

**Steps to build the index** (one build is one commit):
1. `git ls-tree -r -z --full-tree <commit>` lists the files (trees are local in the blob-less clone). There is no `--long`: sizes are unknown there, and asking for them would read every blob. The OIDs listed are remembered, so `/blobs/{oid}` serves them from now on. (A commit that is complete is not listed again, so a request to build it notes none. That leaves no gap: the OIDs the page asks for come with the file list of the pull request and the tree of its head, which note theirs.)
2. Keep regular files (modes 100644 and 100755) of a supported language, leaving out `node_modules` directories and `*.min.js`. A commit with more than 100,000 of them fails: "the commit has N source files; the index covers at most 100000".
3. Ask the index which of these blobs it holds already. A blob noted `unreadable` is tried again, but only by builds of commits that are not complete yet.
4. **Downloading files.** List the blobs the repo lacks with `git rev-list --objects --no-walk --missing=print --no-object-names <commit>` under `-c protocol.allow=never`, which never fetches by itself (S18). Fetch, in this order and in one phase with one count:
   - the blobs to parse. A fetch that fails ends the build.
   - the blobs the index already holds tags for. The lines of references are cut from the cache repo's copy, and a second clone of the repository, or one made anew, lacks the blobs the first one gave the index. This part is best effort: a failure is logged, the chunks after it are skipped and the build goes on.

   Each list goes in chunks of clamp(n / 20, 500, 2000) blobs, and each chunk in a task that owns the repo lock for that chunk only, so pull requests opened meanwhile are not kept waiting. The fetch is the one of step 6 of 5.1, and it gives up on a transfer that stays under 1000 bytes a second for a minute (`http.lowSpeedLimit`, `http.lowSpeedTime`). A blob the remote does not give is noted `unreadable`.
5. **Parsing.** The sizes of the blobs come from `cat-file --batch-check`, run only on blobs that are local (a size is not known before: a blob that turns out too large was fetched once, and is not again). A blob over 1 MiB is noted `too_large` and not parsed; one that cannot be sized is noted `unreadable`. The rest are read through `cat-file --batch` in groups of at most 16 MiB and parsed by min(cores, 4) threads. A file that takes more than 1.5 seconds to parse and match is given up on: it is noted `unreadable`, without symbols. A group is stored in the background while the next one is read and parsed.
6. **Saving.** A `.h` file is C or C++ by its content (see Scope): where its blob is stored under both grammars, the content decides. Only blobs that are local, whose size the index knows and that are no larger than 1 MiB are read for this, in groups of at most 16 MiB. Then the listing of the commit (`tree_entries`) and the mark that it is complete are written in one transaction, so a lookup never sees a half-indexed commit.

The phases only move forward, and the count is that of the phase: blobs fetched while fetching, blobs parsed while parsing. A phase with nothing to do is skipped.

**Cancelling.** A cancel is looked for at the start, before each fetch chunk (and again once the repo lock is held), before each group of blobs is read, while a file is parsed and matched (tree-sitter calls back every few hundred steps, and the loop that follows looks again every 4096 tags) and before the last step. Git is never killed by a cancel: the fetch that has begun finishes, or is given up on by the low-speed guard, and the build stops after it. What was stored stays, so a retry does only what is left.

**The index is keyed by blob OID, not by commit.** Files with the same content have the same OID.
So when the same repo moves to another head or another PR, only the new blobs need processing, and the increment is small.
A blob is stored once for each grammar that names one of its paths, however many commits and paths hold it.
- `parsed`: it has its tags. `too_large`: over 1 MiB, no tags. `unreadable`: the remote did not give the blob, or it took too long to parse. The commit is listed meanwhile, with that file having no symbols; a later build tries it again, and a parse that succeeds replaces the note.
- An insert leaves a stored blob alone (`ON CONFLICT (oid, grammar)`), except that it replaces an `unreadable` row by a better one. So two jobs, or two servers, storing the same blob do no harm.
- A status this version does not know, written by a newer one, counts as `held`: the file is listed and not parsed again, but nothing is known of its tags, so a build neither fetches the blob for the lines of references nor reads it to place a `.h` file. A lookup that finds a reference in a held blob fetches it for the line like any other. A request for the symbols of a held file parses it each time, as the row is never replaced.

**Jobs.** One commit is built at a time, and the others wait in a queue.
- A request to build a commit that is complete already answers `ready` and runs no job. Otherwise it joins the job that builds the commit, unless that job failed, was cancelled or is being cancelled: a new job takes its place, and it waits for the turn the old one gives up. A job that gets its turn and finds the commit complete (another job finished it) ends `ready` at once.
- A job is `missing` (no job, and no complete index), `queued`, `building`, `ready`, `failed` or `cancelled`. Jobs that are ready are forgotten (the database tells). Failed and cancelled ones are forgotten oldest first once there are more than 64. A build whose task stops without an answer is marked failed ("the indexing task stopped").
- When the server exits, it cancels every job first, and tells the fetches for the lines of references to stop (see Looking names up). A job in the queue leaves it, and one that starts afterwards fails with "indexing is shutting down". It waits up to 30 seconds for the tasks (the fetch that has begun is waited for, not killed), and at that deadline aborts what is left.

**Storage.** `index.sqlite` in the cache directory, with mode 0600, in WAL mode (`synchronous=NORMAL`), migrated with `PRAGMA user_version`. One connection writes, one answers the lookups, and a third answers a build's question of which blobs are stored, a long read that would keep the lookups waiting. Nothing is evicted, but the index is emptied when its tags were made by older queries, since a stored blob is never parsed again. `TAGS_VERSION` in `src/index/tags.rs` is raised with every change to a query, a grammar, or how tags are made, that changes the tags of some file (a test holds a hash of the queries and of the node types of the grammars, and fails until it is updated), and `meta` holds the version of the tags on disk. On opening, an index of a lower version, or of none, loses its blobs, names, tags and commits in one transaction, and builds again as commits are looked up; the blobs that are not in the cache repo are fetched again. An index of a higher version, or of a newer schema, is left alone. A server of an older version, still running or started later, is not told: it adds tags of its own version, which stay until the version is raised again. A lookup refreshes the `used_at` of its commit when it is more than an hour old, for a future cleanup. If the file cannot be opened, the server says "code navigation index is disabled" on stderr, the index endpoints answer 503 `index_unavailable`, and symbols are still parsed on demand, without being kept.

```sql
CREATE TABLE blobs (id INTEGER PRIMARY KEY, oid TEXT NOT NULL, grammar TEXT NOT NULL, status TEXT NOT NULL, size INTEGER NOT NULL, UNIQUE (oid, grammar));
CREATE TABLE names (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
CREATE TABLE tags (blob_id INTEGER NOT NULL, name_id INTEGER NOT NULL, kind TEXT NOT NULL, is_def INTEGER NOT NULL,
                   line INTEGER NOT NULL, col INTEGER NOT NULL, end_col INTEGER NOT NULL,
                   scope_start INTEGER NOT NULL, scope_end INTEGER NOT NULL, snippet TEXT NOT NULL, snippet_col INTEGER NOT NULL);
CREATE INDEX tags_name ON tags (name_id, is_def, blob_id);
CREATE INDEX tags_blob ON tags (blob_id) WHERE is_def = 1 OR kind = 'implementation';
CREATE TABLE commits (id INTEGER PRIMARY KEY, oid TEXT NOT NULL UNIQUE, complete INTEGER NOT NULL, files INTEGER NOT NULL, used_at INTEGER NOT NULL);
CREATE TABLE tree_entries (commit_id INTEGER NOT NULL, path TEXT NOT NULL, blob_id INTEGER NOT NULL, family TEXT NOT NULL,
                           PRIMARY KEY (commit_id, path)) WITHOUT ROWID;
CREATE INDEX tree_entries_blob ON tree_entries (commit_id, blob_id, family);
CREATE TABLE meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL) WITHOUT ROWID;
```

Names are stored once (`names`). Lines are 1-based and columns are 1-based UTF-16 code units, which is what Monaco counts: tree-sitter reports byte offsets, so they are converted. A line ends at `\n`, which covers LF and CRLF files; a lone `\r` is not a line break, and a byte order mark is ignored. A definition (and a Rust `impl` block) keeps the trimmed text of its line, at most 200 UTF-16 units, as its `snippet`; the line of any other reference is cut at lookup time (below).

**Looking names up.** The endpoints are in section 6. They take the head the page shows, which must be the pull request's current head (409 `stale_head` otherwise), and `side`: `head`, or `base` for the merge base. `path` is the file of the click as that commit has it (on the base side, a renamed file has its old path), and its extension names the language family. They never start a build: a commit whose index is not complete gets 409 `index_missing`, and a file of a language the index does not cover gets `language: null` and no hits.
- **Definitions** with the name, in files of the family of `path`. Constructors are left out (they remain symbols of their file): they have the name of their class, so the class would otherwise never be the only definition of `new Foo()`. Order: the same file, then the same directory, then the other files, by how many leading directories they share with the file of the click, then by path and position. Only the nearest 200 are returned, and `truncated` says that there were more. A definition's line is stored in the index, so a lookup of definitions asks the index only and never fetches.
- **References** with the name: calls, type uses, in Rust `impl` blocks, and values, the way a function is wrapped or kept (`deco(f)`, `Thread(target=self.run)`, `items.map(render)`, `return wrapper`, `{ .open = dev_open }`). Order: the same file, then the same directory, then the rest by path and line; the first 1000 are returned, with `truncated` as above. A name read in any other place, such as `this.groupCount` in a condition, is not a reference.
- **The lines of references** are cut from the blobs of the cache repo, in groups of at most 16 MiB by the sizes the index noted. The index may hold a commit that another clone of the repository built, so the cache repo may lack some of these blobs. The lookup tells which with one listing (the one of step 4 of the build, which takes no lock) and fetches those of its hits, best effort, in tasks of their own:
  - One task at a time fetches a blob. The task owns the repo lock for its fetch (the fetch of step 6 of 5.1, with the same guard against a stalled transfer), and goes on if the lookup that started it is dropped or has stopped waiting, so that git is never left running without the lock. A lookup that wants a blob that a task has in flight waits for that task, and queues no fetch of its own for the lock, which a build or a pull request that is loading may hold for long.
  - A lookup waits at most 5 seconds for all the tasks it depends on together. After that it answers without the lines it lacks (an empty `snippet`), and the tasks go on, so that a later lookup finds the blobs.
  - When a fetch fails, the remote tells which blobs it refuses (one that stops at the first it refuses names only that one). Those are not asked for again for 2 minutes (per repository and blob; at most 4096 are remembered, the oldest forgotten first). The task asks again for the others for as long as the last ask gave up a blob and 10 seconds have not passed since it got the lock; the blobs left then are given back, for a later lookup. When the remote names none of the blobs asked for, all are noted.
  - Without a GitHub token the lookup answers without the lines of the blobs the repo lacks, and when the listing fails, without any line; either is told on stderr. When the server exits, the tasks that still wait for the repo lock leave without fetching, and no new one starts.
- **Symbols of a file** (`/blobs/{oid}/symbols`) are parsed on demand, by blob, and kept in the index, where a build finds them too. A file over 1 MiB is answered with `tooLarge: true` and no symbols. So is one that takes more than 1.5 seconds to parse, which is not stored (taking long says something about that run, and a build tries again), but remembered in the server for 10 minutes (at most 256 files), so that repeated requests do not each take a thread for that long.

**Where it is used:**

| Feature | Approach |
|---|---|
| Go to definition | Ctrl+B with the caret on a name, or Ctrl+click on one (Cmd instead of Ctrl on macOS): our own code, not a Monaco `DefinitionProvider`. The click is a press and a release on the same name, so dragging a selection with Ctrl held asks for nothing. While Ctrl is held, the name under the pointer is underlined. A name is an identifier in code: not a keyword of the language (each language has its own list), not a number, and not in a comment, a string or a regular expression; it is at most 256 bytes. Words that are keywords only where they stand (`type`, `from`, `get`, `match`, `union`, `operator`) are names. A keyword can still be a name where the language lets it be one: a Rust raw identifier (`r#type`), and in JavaScript and TypeScript a reserved word that names a member, after a dot (`map.delete(key)`) or where the member is declared (`delete(key) {`, `static new = create()`, `{ default: 1 }`, `import: string`). A declaration is told from its line alone: between the word and the last `{`, `,` or `;` before it (or the start of the line) there are only modifiers (`static`, `async`, `get`, `readonly` and the like), and what follows the word is a `(` or `<` right after it (a method), a `:` or `?:` right after it (a property; after a space, as in `null :`, the colon is taken for a ternary's) or an `=` (a property with a value). A word that starts a statement or an expression (`if`, `return`, `new`, `super`, `import` and the like) is taken for a method only with a modifier before it (`static new(config) {`), except `function` after `async` alone: `async function(req) {` is a function. A keyword in any other use (`if(ok)`, `} catch (e) {`, `return`, `new X`) is not underlined, and clicking on it asks for nothing. A caret on no name, and a file type the index does not cover, each give a toast that says so. The page asks for the definitions (building the index first, see above), and then: none gives a toast; one is gone to; several with exactly one in the file of the click go to that one; otherwise a list "Choose declaration of X (N)" opens, nearest first |
| Usages | There is no key of its own. Ctrl+click or Ctrl+B on the name of a definition itself (the file's symbols say it is one, or the definition lookup has a hit exactly there) is no place to jump to, so it lists the places that use the name, asked under the name the definition has, which may be longer than the word clicked (the `operator` of `operator==`): "Usages of X · matched by name (N)". Each row shows the file, the line number and the line with the name marked; a hit whose line could not be had (see above) shows the file and the number only. The results are approximate, and the list says so: they are matched by name. A list that is cut says so (see The lists), and so does an empty one: "No usages found", with a note that usages are matched by name in the files of the language |
| The lists | A popup anchored at the click or the caret (above it where there is no room below). The first row is active and has the focus. ↑ ↓ Home End PageUp PageDown move, Enter or a click opens the place, Esc or a press outside closes it and the focus goes back to the editor. A name far along its line (more than 22 characters in) is shown from 16 characters before it, with a "…" in front. In a row the directory gives way to the line of code, which keeps at least 40 characters or half the row, whichever is less. The lists that the server cut short (200 declarations, 1000 usages) count "200+" or "1000+" and say under the rows "Only the first 200 declarations are listed", or the same of usages. Everything shown comes from the repository, so it is put on the page as text |
| Where a hit opens | A file the pull request changes opens its diff at the line, on the side of the commit searched: a hit in the base commit opens the old file, and a renamed file's old path is on its base side. A copy does not stand for the file it was copied from. An unchanged file opens read-only from the head tree (it reads the same on both sides). Any other path gives a toast that says it is not one of the pull request's files. A jump is a history entry, so Back returns to where it was left (see 3.3) |
| Sticky header | Our own overlay at the top of each visible editor: both sides in split view, the one editor in unified view. Monaco's sticky scroll is switched off in the diff editor, and under `hideUnchangedRegions` it is force-disabled (S20). The rows are the scopes around the first line in view, outermost first, at most 4 (the innermost are kept), from the symbols of the file; constants, fields, variables and properties are skipped. A row is the line of the scope's name, from column 1, with its line number. Clicking it reveals that line and focuses the editor; the wheel scrolls the code beneath. Rows are chosen for the line that is under them, so a scope leaves the header as its end reaches it, as in JetBrains and VS Code; a row is drawn only while its line is above the line just under the header, so no row shows that line again, nor one that is not reached yet. The rows stay clear of the bars of collapsed unchanged lines, wherever in the file they are, since rows over a bar would hide its controls: as many rows as fit between the top of the view and the next bar, none while a bar is at the top (the bar above the first change is, until the code is scrolled away from it), and all four when no bar is in view. An editor too short for four rows keeps fewer, the innermost scopes. A caret moved by keyboard is scrolled out from under the header, so is one on the line of a row that was clicked, and comment cards revealed top-first land below it. The top bar's "Sticky" toggle (on by default, kept in `localStorage`) turns it off; it has no key. A request for symbols that fails is tried again after 2 and 8 seconds |
| Colors of the header | The rows show plain text first. Colors follow from `monaco.editor.colorize`, which is asynchronous (S20); the HTML it returns is parsed with `DOMParser` into an inert document, of which only the text and the `mtk` token classes are kept, and the rows are built from text nodes (see section 8). A line over 1000 characters is cut and stays plain |
| Outline and breadcrumbs | There is no file outline popup, and Monaco's quick outline (Ctrl+Shift+O) is unbound. The breadcrumbs of collapsed unchanged regions come from a `DocumentSymbolProvider` registered for the `dr` scheme, which nests the symbols of the file by scope |

**Scope:**
- By default, index only the head version. When the user requests a jump on the old file (left side), index the merge-base version on demand.
- Languages: Rust (`.rs`), Go (`.go`), Python (`.py`, `.pyi`), TypeScript (`.ts`, `.mts`, `.cts`), TSX (`.tsx`), JavaScript (`.js`, `.mjs`, `.cjs`, `.jsx`), Java (`.java`), C (`.c`, `.h`) and C++ (`.cc`, `.cpp`, `.cxx`, `.c++`, `.hpp`, `.hh`, `.hxx`, `.h++`, `.h`). Extensions are matched as written, in lower case.
- **Families.** Names are matched within a family: TypeScript, TSX and JavaScript are one, and so are C and C++. Rust, Go, Python and Java stand alone.
- **`.h` files** are C++ when their content has `::`, `public:` or one of the words `class`, `namespace` and `template`, and C otherwise. A header is looked up as C or C++ by the grammar the index holds it under. When the index holds its blob under both, the row that says the most about the tags wins (`parsed`, then `too_large` or a status this version does not know, then `unreadable`; C before C++ among equals), so that a row without tags, such as a stale `unreadable`, does not take a header from the grammar it was parsed under. Only where both grammars have tags does the content decide.
- **The queries are ours.** They live in `src/index/queries/*.scm`. Rust, Go, Python and Java start from the tags query that the grammar's crate ships. C and C++ replace it: functions are tagged on the whole definition (so a scope spans the function and not just its signature), and calls and type uses are references. JavaScript is a base (the upstream query of `tree-sitter-javascript` 0.25.0, minus the `(comment)* @doc .` prefix, whose matching time is quadratic in a run of comments: 10,000 comment lines take a second) plus additions of ours: module-level variables, constructors, `new ns.Foo()` and `class A extends B` as references, class fields holding functions and JSX components. TypeScript and TSX combine that base with the upstream TypeScript query and additions of ours (type aliases, enums, namespaces, references to types). Where several patterns tag the same name, the first one written wins, and a reference that sits on the name of a definition is dropped.
- **Values.** Every query tags a name as a `value` reference where it is passed to a call (in Python also as a keyword argument), returned (in Rust also as the last expression of a function), assigned, used to initialize a variable or a constant, or held in a list, an array, a tuple, a set, or a dict, object or struct literal (the value, not the key; also the shorthand `{ render }` of JavaScript and `Handler { parse }` of Rust). So is a default parameter in Python, JavaScript, TypeScript and C++, a class field or C++ member that a name initializes, a JSX expression (`onClick={handle}`) and the method of a Java method reference (`this::process`). After `.`, `::` or `->` the last name is tagged (`self.run` tags `run`; in C++ behind at most three qualifiers, as for a call). Left out: `self` and `cls` in Python, `None` in Rust, and `&x` (`&Foo::bar` is kept in C++). What is assigned to is not tagged, except the names in a destructuring assignment of Rust (`(a, b) = (b, a)`), and a capitalized path in Rust (`Kind::B`) stays a type. Other places, such as the body of an arrow function or a lambda, a ternary, or the arguments of a Rust macro, are not tagged. A query cannot tell a function from a variable, so variables are tagged too, which makes the index bigger. The tags grow by 80% on the standard library of Python 3.12 (76,000 to 137,000), 90% on the JavaScript and TypeScript of `node_modules`, 45% on the TypeScript of Tideline, 10% on the Rust of tokio, axum, serde and rusqlite, and 4% on 1,500 C headers.
- **The runner is ours too**, over tree-sitter's `QueryCursor` instead of the `tree-sitter-tags` crate (S17). Its wall-clock limit is 1.5 seconds for one file, parsing and matching together, and the cancellation flag is looked at while parsing and matching. Tags nested deeper than 60,000 levels are left out. A tag is kept only if its name is an identifier: names longer than 256 bytes (generated junk), and names with whitespace, control characters, or the separators `.` `,` `;` `:`, brackets, quotes, `\` and `<` `>` in them (the upstream queries also capture things like `ns.Thing` and `make<int>`) are dropped. The one exception is the name of a C++ operator (`operator==`, `operator()`, `operator new[]`, `operator co_await`). It is kept without the whitespace that does not separate two words, so `operator ()` and `operator()` are one name, and `operator new[]` and `operator co_await` keep the one space they need.
- **Tag kinds.** Definitions are `function`, `method`, `constructor`, `class`, `interface`, `enum`, `type`, `constant`, `variable`, `module`, `namespace` or `macro`. References are `call`, `type`, `class`, `implementation` (a Rust `impl` block, which has no name of its own; it is also a scope in the symbols of its file) or `value` (a name used as a value, see above).

## 6. HTTP API

All endpoints start with `/api/{host}/{owner}/{repo}`. The table below omits this prefix.

| Method | Path | Description | Milestone |
|---|---|---|---|
| GET | `/pulls/{n}` | PR info: title, author, state, base and head, merge-base, node id | M0 |
| GET | `/pulls/{n}/files` | File list, with the `ChangedFile` shape below, over the full PR range. `from` / `to` for a commit or a range come later | M0 |
| GET | `/blobs/{oid}` | Raw file bytes, with `Cache-Control: immutable` | M0 |
| GET | `/pulls/{n}/commits` | Commit list | later |
| PUT | `/pulls/{n}/viewed` | Request body `{path, viewed, head}`. Returns 409 `stale_head` when `head` is not the current head | M1 |
| GET | `/pulls/{n}/since-review` | Returns `{from, to, mode: direct \| rebased \| fallback, baseline}` | later |
| GET | `/pulls/{n}/tree?head=` | Every file at the head, `{head, entries: [{path, mode, oid}]}`, for the full project tree. Listed locally with `git ls-tree` (trees are in the blob:none clone) and cached per snapshot; its OIDs become fetchable through `/blobs`. With `head`, a cached snapshot of that head is used however old; if the server has moved on, the response carries the newer `head` and no entries | M1 |
| GET | `/pulls/{n}/threads` | `{head, locked, pendingReview, latestReview, threads}`: review threads, the viewer's pending review and latest submitted review, read fresh from GitHub | M2 |
| POST | `/pulls/{n}/threads` | `{head, path, body, mode: single \| review, subjectType: LINE \| FILE, side?, line?, startSide?, startLine?}`: a new thread, posted at once or added to the pending review (started if needed) | M2 |
| POST | `/pulls/{n}/threads/{t}/replies` | `{head, body, mode}`: a reply | M2 |
| PUT | `/pulls/{n}/threads/{t}/resolved` | `{resolved}`: resolve or unresolve | M2 |
| PATCH / DELETE | `/pulls/{n}/threads/{t}/comments/{c}` | Edit (`{body}`) or delete one of your comments | M2 |
| POST | `/pulls/{n}/review` | `{head, reviewId, event: COMMENT \| APPROVE \| REQUEST_CHANGES, body}`: submit the pending review `reviewId`, or (null) a review of just the summary | M2 |
| DELETE | `/pulls/{n}/review` | `{reviewId}`: discard the pending review | M2 |
| POST | `/markdown` | `{text}` → `{html}`: the text as GitHub renders a comment in the repo, for previews | M2 |
| GET | `/blobs/{oid}/symbols?path=` | `{language, tooLarge, symbols: [{name, kind, line, column, endColumn, startLine, endLine}]}`: the definitions of one file (and a Rust file's `impl` blocks, kind `implementation`), ordered by `startLine`. `path` names the language. `language` is null for a file type the index does not cover. `tooLarge` is true, with no symbols, for a file that is over 1 MiB or took too long to parse. The blob is served where `/blobs/{oid}` serves it. `Cache-Control: no-store` | M3 |
| POST | `/pulls/{n}/index` | `{head, side: head \| base}` → `IndexStatus`, as the next row has it. Starts building the index of the head, or of the merge base; joins the build that is running; or answers `ready` at once. The build goes on without the request. `no-store` | M3 |
| GET | `/pulls/{n}/index?head=&side=` | `IndexStatus`: `{commit, state: missing \| queued \| building \| ready \| failed \| cancelled, phase: listing \| fetching \| parsing \| saving \| null, done, total, cancelling, message}`. `done` and `total` count what the phase works on. `message` says why a build failed | M3 |
| DELETE | `/pulls/{n}/index?head=&side=` | Asks the build to stop. The answer is its `IndexStatus`, which says `cancelling` until it has | M3 |
| GET | `/pulls/{n}/definitions?head=&side=&path=&name=` | `{language, commit, name, hits: [{path, line, column, endColumn, kind, snippet, snippetColumn, tier: file \| dir \| other}], truncated}`: where `name` is defined in files of the language family of `path`, nearest first, at most 200. Asks the index only, and never builds it. `no-store` | M3 |
| GET | `/pulls/{n}/references?head=&side=&path=&name=` | The same shape, for the places that use `name`: at most 1000. It never builds the index, but it may fetch blobs for the lines, and waits at most 5 seconds for them (see 5.8). A hit's `snippet` is its line, cut from the blob; it is empty when the blob did not come in time or could not be had or read | M3 |

Errors are `{"error": {"code", "message"}}`. For writes: 400 `bad_request`; 404 `not_found` (the thread or comment is gone); 409 `stale_head`, `pending_review_exists` or `review_changed`;
422 `rejected` (GitHub refused, with its message); 502 `github_error` (the write may or may not have landed: the client looks before sending again).
A request body over 512 KiB gets a plain-text 413.

For code navigation: 400 `bad_request` for a malformed `head` (a full lowercase commit OID), `side`, `path` (1 to 4096 bytes, no control characters) or `name` (1 to 256 bytes, no control characters and no whitespace, but for single spaces in the name of a C++ operator, such as `operator new[]`);
409 `stale_head` when `head` is not the current head of the pull request, and `index_missing` when the commit is not indexed (completely) yet, which is the cue to build it;
503 `index_unavailable` when the index database could not be opened or failed.

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
- `/{host}/{owner}/{repo}/pull/{n}?file=`: view a PR (`&from=&to=` later, with the commit and range views).
- **History entries (M3).** The app writes `history.state = {id, depth, pos?}` into every entry it makes, with its own small router (`web/src/router.ts`, `web/src/nav/history.ts`). `id` is random and tells entries apart, also those with the same URL; `depth` counts the entries pushed since the app first saw the tab's history, and Back stays inside the app while it is above 0; `pos` is `{path, side, line, column}`: the place the caret was when the user left the entry (and, once it has rested for a second, while in it), or the place a jump lands on. A jump is a push, so the browser's Back and Forward return to where the user was, and so do Ctrl+Alt+←/→ (3.3). An entry the app did not make gets a state when it is first looked at.

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

**State management:** server data is fetched with plain `fetch`, in the wrappers of `web/src/api.ts`, and held in React state; there is no query library, and the few caches are purpose-built (4.2). UI state lives in the URL (the current file) and in `localStorage` under `dr.` keys: side by side or unified, ignoring whitespace changes, the width of the tree, its scope (changed files or all) and the "Sticky" toggle. Open comment boxes and what is typed in them are kept in `sessionStorage`, per pull request.

## 8. Security

The local server holds a GitHub token and has write endpoints: viewed marks and, from M2 on, review comments and reviews. So it must stop other web pages from reaching it through the user's browser.

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
- **GitHub-rendered HTML (comment bodies and previews) goes through a dedicated DOMPurify instance** before it reaches the page:
  - no `style` (tag or attribute), forms, iframes or form controls; `id` and `name` are dropped (DOM clobbering);
  - only absolute `https:` URLs; links open in a new tab with `rel="noopener noreferrer"`; task-list checkboxes are disabled.

  This is the only place the app sets `innerHTML`.
- **Lines of the repository, in code navigation, are put on the page as text.**
  - The sticky header shows lines of source, colored by Monaco. `monaco.editor.colorize` returns HTML, which is never handed to `innerHTML`: it is parsed with `DOMParser` into an inert document (nothing in it runs or loads), and only its text and Monaco's token classes are kept (`mtk` and a number, or `mtki`, `mtkb`, `mtku`, `mtks`, checked against a pattern). The rows are built from text nodes.
  - The lists of definitions and usages show paths and lines as React text. The name in a line is marked with `<mark>` around a text node.
- **The index follows the rules above and adds few of its own:**
  - Its endpoints sit under `/api/*`, so every check above applies to them: starting or stopping a build is a non-GET request (Origin and `X-DR`), and a lookup is a GET that needs the same-origin signal, though a lookup of usages may make the server fetch blobs (see 5.8). Those are only blobs that the index lists for the references it found, never an OID that the request names. A task fetches them, with the token, and goes on after the request has ended; a blob whose fetch failed is not asked for again for 2 minutes.
  - The client never names the commit to index. It names the head it shows, which must be the pull request's current head (409 `stale_head` otherwise), and `side`; the server indexes the head or the merge base it knows. The token stays on the server, as for any fetch.
  - `index.sqlite` holds names, paths and the line of each definition, so like the repos it is private: the file is created with mode 0600 (its `-wal` and `-shm` files get the same mode), in the cache directory, which is 0700.
- **The app page has a Content-Security-Policy and `Referrer-Policy: no-referrer`**, so a sanitizer miss still can't run script, post a form or load code from elsewhere, and links and images don't leak the local URL:

  `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; font-src 'self' data:; worker-src 'self' blob:; connect-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'`

  Monaco needs inline styles and same-origin (or `blob:`) workers; comment images come from GitHub's https hosts.
- **Review writes:**
  - node ids must match `[A-Za-z0-9_=+/-]{1,128}` and only ever reach GitHub as GraphQL variables;
  - a comment's path must be in the PR's file list;
  - bodies are capped at 65536 characters, request bodies at 512 KiB.

  Ids aren't checked for ownership: the token is the user's own, so a write can do no more than the user can on github.com, and the checks above already keep other pages out.
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

**Measured for M3** (a release build against real pull requests, cold cache, read-only):
- cli/cli#14673 (Go): the first jump built the index of the head in 4.0 s (945 blobs fetched, 955 parsed). `index.sqlite` took 13.6 MB. A definitions lookup took 0.4 ms, a references lookup 9 ms. The index of the merge base (the old side) took 0.3 s more, since it shares most blobs with the head.
- microsoft/vscode#341600 (TypeScript): a cold build of the head was cancelled 8 s in, in the middle of the fetch (3,024 of 15,129 blobs), and stopped 2.2 s later, when the chunk in flight had finished. The restart fetched only the remaining 11,349 blobs, and the index was ready 43 s later (fetch 37 s, parsing 15,127 files about 5 s). `index.sqlite` took 181 MB, 222 MB with the rest of the cache. Definitions and references took 0.3 ms each. The merge base was ready in 0.3 s.
- When this was measured, a property read such as `this.groupCount` was no reference, so the `groupCount` getter of vscode had no usages, and there were no value references, which make the index bigger (see 5.8). A read that is a value (passed, returned, assigned or held) is a reference now; one in a condition still is not.

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
| S6 | Answered by S20 (M3): no. Sticky scroll is force-disabled in the DiffEditor while `hideUnchangedRegions` is on, so the fallback was taken | — | Our own bar at the top that shows the function or class name |
| S7 | Confirmed (M1 phase 0): GraphQL, like REST, returns at most 250 commits, namely the latest 250 that end at head. `totalCount` is the real total | — | Show a notice in the UI when the limit is exceeded |
| S8 | Confirmed (M1 phase 0, Monaco 0.57): view zones align on both sides automatically; only `setSelections` on the new-file editor reliably expands collapsed regions; in unified mode, zones can't be placed on the original-file side; cards stay put when the code scrolls horizontally only if they use a view zone plus an overlay widget; the production build works under the CSP response header | — | Approaches already in place |
| S10 | Confirmed (M1 phase 0): gh's OAuth token (`repo` scope) can call `markFileAsViewed` / `unmarkFileAsViewed`. A path that is not in the PR returns `UNPROCESSABLE`. After a push, viewed files that the push changed become DISMISSED. Fine-grained PATs are untested | — | Writes GitHub refuses (`UNPROCESSABLE`, `FORBIDDEN`) become 422 `rejected` with GitHub's message (M2); the UI rolls back and shows it |
| S12 | Confirmed (M2 phase 0): `addPullRequestReviewThread` takes lines within 3 lines of a change on their side (git's default context; every line of an added file, the old lines of a deleted one), and ranges across hunks. A file-level comment works on any file, pure renames included. Anything else returns `thread: null` without an error | — | A null thread is a 422 `rejected` |
| S13 | Confirmed (M2 phase 0): one pending review per user and PR. While it exists, no single comment can be posted, and new threads and replies go into it. Without one, a reply posts at once. New threads anchor to the current head, even when the pending review sits on an older commit | — | Head checks on line comments; the dialog warns about an older pending review |
| S14 | Confirmed (M2 phase 0): the viewer's pending comments are in `reviewThreads` with `state: PENDING`; `reviews(states:[PENDING], first:1)` returns the viewer's pending review | — | The app reads 5 and keeps the one with `viewerDidAuthor` |
| S15 | Confirmed (M2 phase 0): approving or requesting changes on your own PR fails with `UNPROCESSABLE` and keeps the pending review. Closed PRs still take reviews and replies | — | — |
| S16 | Confirmed (M2): a textarea in an overlay widget over a view zone takes typing, IME and paste, as long as key, input, composition and clipboard events stop at the card | — | — |
| S17 | Confirmed (M3 phase 0): the crates are tree-sitter 0.27.1 with tree-sitter-rust 0.24.2, -go 0.25.0, -python 0.25.0, -typescript 0.23.2, -javascript 0.25.0, -java 0.23.5, -c 0.24.2 and -cpp 0.23.4, with rusqlite 0.40.2 (bundled SQLite). The `tree-sitter-tags` crate was dropped for our own runner over `QueryCursor`: it can be neither cancelled nor given a time limit, and some files took it tens of seconds | — | — |
| S18 | Confirmed (M3 phase 0): `git rev-list --objects --no-walk --missing=print` lists the blobs a blob-less clone lacks without fetching any: 63 ms on microsoft/vscode. `cat-file --batch-check` would fetch the missing objects one at a time on git 2.43. Fetching all of vscode's blobs takes about 34 s in chunks of 2000 | — | — |
| S19 | Confirmed (M3 phase 0): parsing runs at about 1000 files a second per core. On microsoft/vscode, 2.2 million tags took 7.8 s with 4 workers and made a 150 MB `index.sqlite`; a references lookup took 18 ms at the 95th percentile, and a definitions lookup of `dispose` 5.7 ms | — | — |
| S20 | Confirmed (M3 phase 0): Monaco's built-in sticky scroll is force-disabled in the diff editor while `hideUnchangedRegions` is on, so the sticky header is our own overlay (z-index 4, above the comment cards). `showOriginalLine` under-reveals on the original side. `getLineTokens` is internal, so the header colors its rows with `monaco.editor.colorize`, which is asynchronous: `colorizeModelLine` forced synchronous tokenization, about 45 ms for line 24,000 of a 25,000-line file. The HTML that comes back is parsed with `DOMParser` into an inert document, and only the text and a whitelist of `mtk` classes are kept. A `DocumentSymbolProvider` on the scheme `dr` gives the breadcrumbs of collapsed regions. Answers S6 | — | — |
| S21 | Confirmed (M3 phase 0): Ctrl+Alt+←/→ reach the page in Chromium. GNOME and KDE may grab them for switching workspaces | — | The browser's own Back and Forward (Alt+←/→, the mouse buttons) do the same, as every jump is a history entry |

## 11. Open questions

- **Binary name:** tentatively `gh-tideline`. That way it can run as a gh extension via `gh tideline`, and also standalone.
- **Default port:** browser extensions and bookmarks need a fixed port. Tentatively a fixed, configurable port, such as 7457.
- **How the server runs:** exit automatically after N idle minutes, or run all the time as a systemd user service? The former costs less. The latter keeps the browser extension usable at any time.
- **Cache eviction:** provide a `gh tideline gc` command that evicts the least recently used entries based on cache size. But "the head at the last review" must be kept. The code index (`index.sqlite`) is never evicted yet (only emptied when the tags change, see 5.8), and it grows with the repositories used (181 MB for microsoft/vscode); each commit in it carries a `used_at` for such a cleanup.
- **Curved connector bands:** after M3, should we draw our own layer on top of Monaco to get JetBrains-style curved connector bands?
