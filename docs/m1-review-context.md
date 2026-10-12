# M1 review 上下文：实现约定

- 状态：草案，待确认
- 日期：2026-10-12

本文档是 `docs/design.md` 中 M1 的落地范围，也是后端和前端之间的接口契约。M0 的约定（`docs/m0-skeleton.md`）继续有效，本文只写新增和改动的部分。

## 范围

**做（对应 design.md §5.3–5.6）：**
1. **已看状态**：和 GitHub 双向同步；文件树勾选框、DISMISSED 高亮、已看进度；`v` 快捷键。
2. **评论线程（只读）**：行内 view zone、文件级评论、过时和已解决的线程单独列出、文件树显示评论数。
3. **按提交或范围查看**：提交选择器，单个提交或连续范围，状态写在 URL 上。
4. **上次 review 之后改了什么**：能处理 rebase。
5. **本地状态库** `state.sqlite`：给第 4 项保存"上次在本工具里看完的 head"。

**这一版不做：**
- 任何写操作，除了标记 / 取消已看（design.md §2 不变）。
- SSE、常驻 `cat-file --batch` 进程、Shiki、换行符和编码检测：仍按 M0 的决定推迟。
- 首页的"最近看过的 PR"列表、`gh tideline gc`：不属于 M1。
- React Query：M0 没用，M1 也不引入，沿用现有的 fetch + 组件状态写法。

## 阶段 0：技术验证（先做，结论写回 design.md §10）

| 编号 | 要验证什么 | 怎么验证 | 不行的话怎么办 |
|---|---|---|---|
| S7 | GraphQL `pullRequest.commits` 是否也有 250 条上限 | 找一个超过 250 个提交的公开 PR | 返回 `truncated: true`，界面提示"只显示最近 N 个提交" |
| S8 | Monaco DiffEditor：① 左右两侧加 view zone 后能否自动对齐；② 对被 `hideUnchangedRegions` 折叠的行调用 `setSelections` 能否展开（已读源码：`hideUnchangedRegionsFeature` 只忽略 ContentFlush 引起的光标变化，预期可行）；③ 单栏模式下能否在原文件一侧放 view zone；④ view zone 放在 `.lines-content` 里，宽度等于 scrollWidth，评论卡片会跟着代码横向滚动、被最长的行撑宽 | 在 `pnpm dev:mock` 里写原型 | ①只在新文件一侧放 zone；②对这些文件关掉 `hideUnchangedRegions`；③见下文单栏兜底；④卡片用 `position: sticky; left: 0` 并限宽为编辑器可见宽度，不行就学 VS Code 的 ZoneWidget：view zone 只占高度，内容放在 overlay widget 里，按 `onDomNodeTop` 定位 |
| S9 | design.md S2 的剩余部分：① force-push 前的旧提交被 GitHub 回收后，`git fetch` 报什么错；② 对这样的提交调用 `compare` 返回什么 | 找一个很久以前被 force-push 过的 PR | 按实测的错误文本分类，归到 `unavailable` / `fallback` |
| S10 | `markFileAsViewed` 在 gh 的 OAuth token 和 fine-grained PAT 下的权限要求，以及无权限时的错误形状 | 在自己仓库的测试 PR 上试（**会改写 GitHub 上的已看状态**） | 把错误映射成 `github_error`，界面回滚并提示权限不足 |
| S11 | `reviews(last:1, author:, states:)`：① `commit` 什么时候为 null；② 作者 push 之后，用户只在线程里回复一句，会不会生成一个 `commit` 等于新 head 的 COMMENTED review（那样"上次 review 之后"就变成空的） | 同 S9 的 PR；自己的测试 PR | ① null 时退到本地基线；② 如果会：改成取最近几次 review，跳过正文为空、且所有评论都是回复（`replyTo` 不为 null）的 COMMENTED review |

**验证结论（2026-10-12）：**
- **S7：有上限，而且是"最近的 250 个"。** llvm/llvm-project#121197（385 个提交）和 git/git#2363（931 个提交）上，GraphQL 和 REST 都只返回 250 个，最后一个是当前 head，缺的是最早的那些；`totalCount` 给的是真实总数。所以 `truncated = totalCount > 返回的数量`，提示文字是"只显示最近 250 个提交"。
- **S9：旧提交很难被回收。** cli/cli 2020 年被 force-push 掉的三个提交，今天仍能按 SHA 浅抓取，`compare` 也正常。不存在的提交：`git fetch` 报 `fatal: remote error: upload-pack: not our ref <sha>`，`compare` 返回 404。按这两个特征归到 `unavailable` / `fallback`。
- **S10：gh 的 OAuth token（`repo` scope）可以调用 `markFileAsViewed` / `unmarkFileAsViewed`**，修改立即生效。路径不在 PR 里时返回 `type: UNPROCESSABLE`、`"Filepath must be part of pull request"`；节点 id 不对时返回 `NOT_FOUND`。push 新提交后，改到的已看文件变成 `DISMISSED`；对 DISMISSED 的文件取消标记得到 `UNVIEWED`。没有 fine-grained PAT，未测。
- **S11②：会。** push 之后只在线程里回复一句，就会生成一个正文为空、`commit` 等于新 head 的 COMMENTED review，它唯一的评论 `replyTo` 不为 null。所以采用兜底方案：查询改成 `reviews(last:20, …) { nodes { body commit { oid } submittedAt comments(first:20) { nodes { replyTo { id } } } } }`，从新到旧取第一个"不是纯回复"的 review。S11① 没能复现，保留"null 时退到本地基线"。
- 顺带确认：过时的线程 `line` 为 null、`originalLine` 有值，第一条评论的 `diffHunk` 可用。
- **S8（Monaco 0.57，无头 Chromium 实测）：**
  - ① 能对齐：只在任意一侧加 zone，另一侧会自动补上同样高度的空白。
  - ② 新文件编辑器上 `setSelections` 能展开折叠区（两侧一起展开，不滚动）；**原文件编辑器上不行**（0.57 的 `showOriginalLine` 算错了展开范围）。所以 LEFT 的行先换算成新文件的行号，统一在新文件编辑器上选中。另外两个坑：zone 挂在第 N 行下面时，第 N+1 行被折叠也会把 zone 藏起来，所以要同时选中 N 和 N+1；zone 如果在展开之前加，展开后另一侧不会补空白，所以必须先展开、再加 zone。
  - ③ 不行：单栏模式下原文件编辑器只是一条 39px 宽的行号栏，zone 被裁掉。改为把 zone 加在新文件编辑器上（位置见下文"评论线程"）。
  - ④ `position: sticky` 不行，ZoneWidget 的做法可行：view zone 只占高度，卡片放在 overlay widget 里，按 `onDomNodeTop` 定位，横向滚动时不动，宽度等于编辑器可见宽度。
  - 点击：卡片直接放在 view zone 里时，点击和选中文字都会落到代码层上（要加 `z-index`）；放在 overlay widget 里不需要额外处理，链接、按钮、选中文字都正常，也不会把焦点抢到编辑器。不要设 `suppressMouseDown: true`。
  - CSP：按下文的响应头跑生产构建，没有违规事件；worker 是独立文件，字体和动态插入的 `<style>` 都正常。

## 本地状态库

- 位置：`$XDG_CACHE_HOME/tideline/state.sqlite`，文件权限 0600，所在目录已经是 0700。
- 依赖：`rusqlite`（`bundled` feature，不依赖系统 libsqlite3）。M2 的符号索引也要用它。
- 只在服务进程里打开一个连接，用 `std::sync::Mutex` 包起来；所有调用都很小，在 `spawn_blocking` 里执行。
- 打开时设置 `journal_mode=WAL`、`busy_timeout=5000`（多个服务实例可能共用缓存目录）。
- 用 `PRAGMA user_version` 做表结构版本号，启动时按版本迁移。
- 打开失败（比如目录只读、文件损坏）时打印一条警告，服务照常运行：已看勾选不受影响，只是不记 head。

```sql
CREATE TABLE seen_head (
  host TEXT NOT NULL, owner TEXT NOT NULL, repo TEXT NOT NULL, number INTEGER NOT NULL,
  head_sha TEXT NOT NULL,
  recorded_at INTEGER NOT NULL,            -- Unix 秒
  PRIMARY KEY (host, owner, repo, number)
);
```

**什么时候写：** 一次 `PUT /viewed` 成功之后，如果用户能勾选的文件（我们的文件列表里、GitHub 也列出的路径）除生成文件以外全部是 VIEWED，就把当前 head 记下来，同时写 `refs/dr/pull/<n>/c/<head>` 保护它。
- 只标记了一个文件就记 head 是错的：之后会被当成"整个 head 都看过了"，用户没打开过的文件在 since-review 里就被藏掉了。
- 只有 GitHub 列出的路径（比如我们识别成重命名、GitHub 当成删除加新增时的旧路径）没有勾选框，不要求它们。
- 缓存里的已看状态可能过时（另一个标签页、github.com 上改过），所以写之前再查一次 GitHub 的完整文件列表确认。要求的路径在新列表里找不到（比如列表不完整）也算没看完。
- 库里已经记的就是这个 head 时不再查，所以每个 head 只确认一次。
- 这一步在响应之后的后台任务里做；服务退出时会等这些任务结束。

## git 层

**铁律：任何 OID 交给 git 命令之前，必须已经抓到本地。** 在 blob:none 的部分克隆里，git 碰到缺失对象会发起不带 `--depth` 的按需下载，一次就可能把整段历史拉下来（实测：`cat-file -e <缺失的提交>` 会下载它的全部祖先）。
- 检查对象是否在本地时，一律加 `-c protocol.allow=never`（和 M0 的 `read_object` 一样），缺失时立即失败，不访问网络。
- `diff-tree`、`merge-tree`、`check-attr --source` 的每个修订参数，都要先确认在本地或已经按 SHA 浅抓取过。

**ref 命名：** 沿用 M0 的 `refs/dr/pull/<n>/{base,head}`，新增 `refs/dr/pull/<n>/c/<sha>`：给提交视图和"上次 review"用到的每个提交。
- 提交已经在本地、跳过抓取时，也要用 `update-ref` 补写这个 ref，否则"旧 head 有 ref 保护"（design.md §4.4）就不成立。
- 重放得到的目录树 T 不写 ref：它随时可以重新算出来，而 gc 不在 M1 范围里。

**新增操作（都在仓库锁下执行）：**
- `ensure_commits(&[sha])`：按需调用，只处理这次请求用到的提交（比如某个范围的 `from` 和 `to`），不预先抓取 PR 的全部提交。
  - 用 `-c protocol.allow=never cat-file -e` 找出本地缺失的；
  - 缺失的用一次 `git fetch --depth=1 --filter=blob:none --no-tags --no-write-fetch-head --recurse-submodules=no --stdin origin` 抓取，refspec `+<sha>:refs/dr/pull/<n>/c/<sha>` 从 stdin 传入，不受命令行长度限制；
  - 本地已有的补写 ref。
- `replay(auth, m_old, m_new, h_old)`：`git merge-tree --write-tree --merge-base=<m_old> --end-of-options <m_new> <h_old>`。三个提交都必须先经过 `ensure_commits`。需要 `GitAuth`，因为它会批量预取所需的 blob（已实测只预取一次）。
  - 退出码 0：第一行是 T 的 OID；
  - 退出码 1：有冲突，返回 `Conflict`；
  - 其他：`GitFailure`。
  - 输出解析写成纯函数放在 `git/parse.rs`，配单元测试。

**通用的范围流水线：** 把 M0 `load()` 里的第 4–7 步抽成 `range_files(from, to)`，M0 的完整 PR 范围也走它：
1. 收集 `diff-tree --no-renames` 里所有改动的 blob，加上 `to` 里相关的 `.gitattributes`；
2. **去掉本地已有的 OID，再批量抓取**。`from` 是 T 时这一步必不可少：`merge-tree` 新写出的合并结果 blob 只存在本地，送去 `fetch --stdin` 会让 GitHub 返回 `not our ref`，整批（包括正常的 blob）都抓不到。
   - 列出缺失对象用 `git -c protocol.allow=never rev-list --objects --missing=print --end-of-options <T>`，输出里 `?` 开头的行就是缺失的 OID，不会触发按需下载。
   - 不能用 `cat-file --batch-check`：它遇到第一个缺失对象就中止。
3. `diff-tree -M --numstat`；
4. `check-attr --source=<to>`；
5. `remember_oids`，让 `/blobs` 接口可以按需下载这些 blob。

已经算过的 `(from, to)` 对记在 `RepoState.blobs_fetched` 里（M0 已有），不再重复抓取。

## GitHub 层

**PR 查询（改）：** 在 M0 的查询里加上：
- `viewer { login }`
- `files.nodes.viewerViewedState`

**只取 head（新）：** `pullRequest(number:$number) { headRefOid }`，给 `PUT /viewed` 校验用。

**提交列表（新）：**
```graphql
pullRequest(number:$number) {
  commits(first:100, after:$cursor) {
    totalCount
    pageInfo { hasNextPage endCursor }
    nodes { commit { oid messageHeadline authoredDate author { name user { login } } parents(first:2) { nodes { oid } } } }
  }
}
```
所有 OID 都用 `check_oid` 校验。

**评论线程（新）：**
```graphql
reviewThreads(first:100, after:$cursor) {
  pageInfo { hasNextPage endCursor }
  nodes {
    id isResolved isOutdated path subjectType line startLine originalLine diffSide startDiffSide
    comments(first:50) { pageInfo { hasNextPage endCursor }
      nodes { id author { login } bodyHTML createdAt url diffHunk } }
  }
}
```
- 某个线程的评论超过 50 条时，用 `node(id:$threadId) { ... on PullRequestReviewThread { comments(first:100, after:$c) {…} } }` 继续翻页。
- `diffHunk` 只取第一条评论的，过时的线程靠它显示上下文。

**已看（新）：** `markFileAsViewed(input:{pullRequestId, path})` / `unmarkFileAsViewed(...)`。

**上次 review（新）：**
```graphql
pullRequest(number:$number) {
  reviews(last:20, author:$viewer, states:[APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED]) {
    nodes { state body commit { oid } submittedAt comments(first:20) { nodes { replyTo { id } } } }
  }
}
```
从新到旧取第一个不是"纯回复"的 review。纯回复的定义：`state` 是 COMMENTED、`body` 为空、至少有一条评论并且所有评论的 `replyTo` 都不为 null（见 S11 结论）。

## 缓存

- **PR 数据**（M0 的 30 秒缓存）里新增 `viewer`、每个文件的已看状态。已看状态放在 `Mutex<HashMap<path, ViewedState>>` 里，`PUT /viewed` 成功后原地更新，这样 30 秒内再次请求 `/files` 不会读到旧值。
- **提交列表、评论线程**：各自按 PR 缓存 30 秒，规则和 M0 一样（`?refresh=1`、2 秒合并窗口、独立任务里执行）。前端刷新时一起带 `refresh=1`。
- **范围文件列表**：按 `(from, to)` 缓存在内存里，不过期（两端都是不可变对象），上限 64 项，LRU 淘汰。
- **since-review 结果**：按 `(H_old, H_new, base_oid)` 缓存。只有没出错、status 为 `ok` 的结果才长期缓存；因为网络错误、限流、抓取失败等临时原因得到的 `unavailable` 或 `fallback`，最多缓存 30 秒。

## 上次 review 之后的计算

```
1. 基线 H_old：最近一次不是纯回复的 review 的 commit.oid；没有（或 commit 为 null）就用 seen_head；都没有 → status = "none"
2. H_old == H_new → status = "unchanged"
3. ensure_commits([H_old])；抓不到 → status = "unavailable"
4. M_old = compare {base_oid}...{H_old} 的 merge_base_commit；compare 失败 → mode = "fallback"
5. M_old == M_new → mode = "direct"，范围 H_old → H_new
6. 否则 ensure_commits([M_old])，抓不到 → mode = "fallback"
7. replay(M_old, M_new, H_old)：成功 → mode = "rebased"，范围 T → H_new；冲突 → mode = "fallback"
8. fallback 的范围：H_old → H_new
9. 范围两端的目录树相同（比如只是 rebase，没有别的改动）→ status = "unchanged"
```

- **不检测 base 分支有没有被改过。** 即使 PR 中途换了目标分支、或者 base 被 force-push 过，用 M_old = merge-base(当前 base, H_old) 重放得到的结果，也不会比 H_old → H_new 的 fallback 更乱，所以没必要为此强制走 fallback。
- 第 4–9 步的判断写成纯函数（输入：各个 OID、各步的结果；输出：status、mode 和范围），配单元测试。
- replay 的真实行为用本地 git 仓库做集成测试：在临时目录里造出"不冲突的 rebase"和"冲突的 rebase"两种历史，并覆盖"冲突文件里合并结果 blob 只在本地"这种情况（见范围流水线第 2 步）。

## HTTP API

在 M0 的基础上新增和修改（前缀 `/api/{host}/{owner}/{repo}` 省略）：

| 方法 | 路径 | 返回 |
|---|---|---|
| GET | `/pulls/{n}` | `PrSummary`，新增 `viewer` 字段 |
| GET | `/pulls/{n}/files` | `ChangedFile[]`，新增 `viewed` 字段 |
| GET | `/pulls/{n}/files?from=&to=` | `ChangedFile[]`，没有 `viewed` 字段（前端按路径从完整列表里取） |
| GET | `/pulls/{n}/commits` | `PrCommits` |
| GET | `/pulls/{n}/threads` | `ReviewThread[]` |
| PUT | `/pulls/{n}/viewed` | 请求体 `{ path, viewed: boolean, head }`；返回 `{ path, viewed: ViewedState }` |
| GET | `/pulls/{n}/since-review` | `SinceReview` |

**`from` / `to` 的校验（防止借这个接口抓取任意对象、写满磁盘）：**
- 两个都必须是合法 OID，并且必须在这个 PR 的白名单里：PR 的 merge-base、head、每个提交及其父提交，以及本进程为这个 PR 算出的 H_old 和 T。
- 不在白名单里：400 `bad_request`。只给其中一个：400。
- 白名单从缓存的提交列表构造；服务重启后第一次请求会先拉提交列表。T 要等 `/since-review` 算过一次才会进白名单（前端总是先请求 `/since-review`）。

**`PUT /viewed` 的校验：**
- 路径必须出现在 **GitHub 自己的文件列表**（GraphQL `files`）里，否则 400。我们本地 `-M` 识别出来的重命名可能和 GitHub 的不一样；已看状态也一律按 GitHub 的路径对应。
- `head` 是前端当前显示的 head。服务端先重新查一次 `headRefOid`；不一致时返回 409 `stale_head`，不调用 mutation。否则 GitHub 会把用户没看过的新改动也标记为已看，DISMISSED 提示就丢了。
- 路径校验用缓存里这个 head 的文件列表，过了 30 秒的 TTL 也照用，不为一次勾选重新加载整个 PR；缓存里没有这个 head 才强制重新加载（没过期的缓存也可能还是旧 head）。
- 已知的空窗：查完 head 到 mutation 生效之间如果正好有 push，新改动会被标记为已看。GitHub 的 mutation 没有前置条件，没法完全避免。
- 照常经过 M0 的非 GET 检查（`Origin` + `X-DR: 1`）。
- GitHub 返回权限错误时用 `github_error`，`message` 里带上 GitHub 的原文。

**新增错误码：** `stale_head` → 409。

```ts
type ViewedState = "VIEWED" | "UNVIEWED" | "DISMISSED"

interface PrSummary { /* M0 字段 */ viewer: string }
interface ChangedFile { /* M0 字段 */ viewed?: ViewedState }

interface PrCommit {
  oid: string
  parents: string[]                 // 第一个是第一父提交
  headline: string
  author: string                    // user.login，没有就用 author.name
  authoredDate: string              // ISO 8601
}
interface PrCommits { commits: PrCommit[]; total: number; truncated: boolean }

interface ReviewComment { id: string; author: string; bodyHtml: string; createdAt: string; url: string }
interface ReviewThread {
  id: string
  path: string
  subjectType: "LINE" | "FILE"
  side: "LEFT" | "RIGHT" | null     // FILE 线程为 null
  startSide: "LEFT" | "RIGHT" | null
  line: number | null               // 整个文件里的行号；过时或 FILE 线程为 null
  startLine: number | null          // 多行评论的起始行
  originalLine: number | null
  diffHunk: string | null           // 第一条评论的 diffHunk
  isResolved: boolean
  isOutdated: boolean
  comments: ReviewComment[]
}

interface SinceReview {
  status: "ok" | "none" | "unchanged" | "unavailable"
  baseline?: { sha: string; source: "review" | "local"; at: string }
  mode?: "direct" | "rebased" | "fallback"
  from?: string                     // H_old 或 T
  to?: string                       // 当前 head
  reason?: string                   // fallback / unavailable 的原因，直接显示给用户
}
```

## 前端

**新依赖：** `dompurify`。

**URL：** `/{host}/{owner}/{repo}/pull/{n}?file=&from=&to=` 或 `?file=&range=since-review`。
- 没有 `from`/`to`/`range`：完整 PR 范围。
- `range=since-review` 由前端解析：先请求 `/since-review`，再用它返回的 `from`/`to` 请求 `/files`。不把 T 写进 URL，因为 T 只存在于这台机器的缓存里。
- **切换文件时保留范围参数。** 现在的 `fileQuery()`（`web/src/router.ts`）和 `PrPage.tsx` 里的跳转只写 `?file=`，要改成保留 `from`/`to`/`range`。
- 切换范围用 `pushState`，浏览器后退能回到上一个范围。
- 切换范围时如果当前文件仍在新列表里，保持不变；否则打开第一个文件。

**提交选择器（顶栏）：**
- 按钮显示当前范围：`全部改动` / `abc1234 标题` / `abc1234..def5678（3 个提交）` / `上次 review 之后`。
- 下拉列表第一项是"上次 review 之后（abc1234）"，status 不是 `ok` 时置灰，悬停显示原因或"没有新改动"。
- 然后是全部改动，再往下是每个提交：短 SHA、标题、作者、相对时间。
- 点击选单个提交，Shift+点击选连续范围。范围 [A..B] 对应 `from = A 的第一父提交`、`to = B`。
- 没有父提交的提交（根提交）不能选。
- `truncated` 时在列表顶部提示"GitHub 只返回最近 250 个提交（共 N 个）"。
- 非完整范围时顶栏显示一条横幅：说明当前范围；mode 是 `fallback` 时加上"包含 base 分支的变化"和 `reason`。

**已看：**
- 文件树每行加一个勾选框；DISMISSED 用 Numbers 色 `#F76D47` 标出文件名，悬停提示"看过之后又被修改"。
- 侧栏底部显示进度：`已看 3/7` 加进度条，生成文件也计入。
- diff 面板顶部的文件标题栏加 `[ ] Viewed` 勾选框。
- `v`：切换当前文件的已看状态；标记为已看时同时跳到下一个未看的文件（按文件树顺序，到末尾就从头找）。
  - 生成文件排在后面：先找不是生成文件的，都看完了才跳到生成文件。
  - 开着 Caps Lock 或用非拉丁键盘布局时也能用（按物理键位 `KeyV` 判断）。输入法正在组字时（`Process`、keyCode 229）不响应。
  - 焦点在输入框（文件查找框、Monaco 的查找框等）里时不响应。判断方法：事件目标是文本类的 `input`、`textarea`、`select` 或 `contenteditable`；Monaco 编辑区（`.monaco-editor`）里的输入元素不算，但它的查找框（`.monaco-inputbox`）算。勾选框不算输入框。
- 先改界面再发请求；失败就回滚，并显示提示条（新增一个通用的 toast 组件，3 秒后消失；带按钮的提示条不自动消失，有关闭按钮）。
  - 409 `stale_head` 时提示"PR 已经更新，请先刷新"，并附一个刷新按钮。
- 同一个文件连续点击时，只保留最后一次的意图：上一个请求完成后，如果最终状态和期望不一致再补发一次。
- 同一个 head 只有一个同步对象：刷新时把服务端的已看状态合并进来，但有请求在途或排队的路径、以及在这次刷新请求发出之后才完成的路径保留本地状态（服务端可能在我们的修改生效前读的 GitHub）。head 变了就重新开始，旧的排队意图和提示条都丢弃。
  - 几次刷新重叠时以最后一次为准；比已经合并过的列表更早发出的列表直接忽略。
  - 新列表里不再出现的路径连同排队的意图一起丢掉，在途请求完成后也不再加回来。
- 只有出现在 GitHub 文件列表里的路径才显示勾选框（完整范围下也可能有路径因为重命名识别不同而对不上）。

**评论线程：**
- 只在完整 PR 范围里显示。其他范围在顶栏横幅里注明"评论只在查看全部改动时显示"。
- 按文件分组；文件树每行显示这个文件的线程数，有未解决的线程时用 Accent 色。
- 没过时、没解决的 LINE 线程：view zone 插在 `line` 那一行下面。左右并排时，`side` 为 LEFT 放在原文件编辑器，RIGHT 放在新文件编辑器。
- FILE 线程：作为 `afterLineNumber: 0` 的 view zone 放在新文件编辑器顶部。
- 过时的、已解决的线程：diff 面板顶部一个"其他讨论（N）"折叠区，默认折叠；每条显示 `originalLine`、"过时 / 已解决"标签，过时的线程再用 `<pre>` 显示 `diffHunk`（纯文本，不当 HTML）。
- **单栏模式下的 LEFT 线程（S8③）：** 全部放在新文件编辑器上，`ordinal: 20000`（默认的 10000 可能排到 diff 自己的"删除行"区块上面）。
  - 落在删除行上：挂在这个改动块的位置，`afterLineNumber` 取 `modifiedEndLineNumber === 0 ? modifiedStartLineNumber : modifiedStartLineNumber - 1`，卡片显示在删除行下面、新增行上面。没法插在两条删除行之间，所以卡片里写明"旧文件第 N 行"和那一行的内容。
  - 落在没改动的行上：用下面的 `origToMod` 换算成新文件的行号。
- **原文件行号换算成新文件行号（`origToMod`）：** 按 `getLineChanges()` 从前往后累加每个在它之前的改动块的行数差（新增行数 − 删除行数）。原型里对全部 196 行没改动的行逐行核对过，没有偏差。写成纯函数并加单元测试。
- **view zone 的实现（S8④，ZoneWidget 做法）：**
  - view zone 只是一个占高度的空 `div`；卡片是一个 overlay widget（`getPosition` 返回 null，自己定位），在 zone 的 `onDomNodeTop` 回调里设置 `top`。
  - 卡片的 `left` 取 `layoutInfo.contentLeft`，宽度是 `width − contentLeft − verticalScrollbarWidth − minimap.minimapWidth`，在 `onDidLayoutChange` 里更新。这样横向滚动时卡片不动，也不会被长行撑宽。
  - 卡片设 `user-select: text`（编辑器根节点禁止选中文字）。zone 用默认的 `suppressMouseDown`。
  - React 组件通过 `createPortal` 渲染进 overlay widget 的 DOM 节点；用 `ResizeObserver` 量卡片高度，变化时更新 `heightInPx` 并 `layoutZone`（原型里调整大小后两次更新就稳定，不会来回循环）。
  - 切换文件、切换左右并排 / 单栏、diff 重新计算后都要重建 zone 和 overlay widget。
  - 每个线程可以折叠成一行（作者 + 第一条评论的开头），展开状态按线程 id 保存在内存里。
- **强制展开（S8②）：** diff 算完后，**先展开、再加 zone**，否则另一侧不会补空白。
  - 把所有有线程的行（LEFT 的先经 `origToMod` 换算）收集起来，在新文件编辑器上调用一次 `setSelections`，每行同时选中 N 和 N+1。不要在原文件编辑器上调用。
  - 然后恢复光标：只有用户移动过光标时才恢复。初始光标在 (1,1)，通常落在顶部的折叠区里，恢复它会把那一块也展开。
- **安全：**
  - `bodyHtml` 用 DOMPurify 清理：禁止 `style` 标签和 `style` 属性，以及 `form`、`iframe`；`href` 只保留 `https:` 开头的绝对地址；清理后的所有 `<a>` 加 `target="_blank" rel="noopener noreferrer"`。
  - **CSP 用 HTTP 响应头，不用 `<meta>`**：由 `assets::serve` 在返回 `index.html` 时加上。原因：`<meta>` 里的 `frame-ancestors` 不生效；而且开发时 `@vitejs/plugin-react` 会往页面里插一段内联脚本，写在 `<meta>` 里会把 `pnpm dev` 弄坏。内容：
    `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; font-src 'self' data:; worker-src 'self' blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'`
    - 已核对生产构建：worker 是 `/assets/` 下的独立文件，CSS 里的字体和图片用 `data:`，打包产物里没有 `eval`。不要加 `'unsafe-eval'`。
    - 评论里的图片来自 GitHub 的 https 域名，所以 `img-src` 放开 `https:`。
    - 同时加 `Referrer-Policy: no-referrer`，加载评论图片时不把本地 URL 带给 GitHub。

**开发代理：** `web/vite.config.ts` 的代理现在只用 `changeOrigin` 改写了 `Host`。从 5173 端口发出的 PUT 带着 `Origin: http://localhost:5173`，过不了 M0 的非 GET 检查，所以代理还要把 `Origin` 改写成后端的地址。

**mock：** `web/mock/` 补上 `/commits`、`/threads`、`PUT /viewed`、`/since-review` 和带 `from`/`to` 的 `/files`，覆盖 rebased、fallback、unavailable、unchanged 几种情况，以及一个 PUT 失败的文件和一个返回 409 的文件，用来测回滚。

## 测试

**Rust（`cargo test`）：**
- GraphQL 响应的反序列化：用录制下来的 JSON 做夹具（提交、线程、评论翻页、viewer、viewedState、reviews 为空和 commit 为 null）。
- `merge-tree` 输出和 `rev-list --missing=print` 输出的解析。
- since-review 的判断表（纯函数）。
- `from`/`to` 白名单校验；`PUT /viewed` 的路径校验、head 校验和非 GET 安全检查。
- 本地 git 集成测试：临时目录里造仓库（`file://` remote + `uploadpack.allowAnySHA1InWant`，模拟 shallow + blob:none），验证：
  - replay 的不冲突和冲突两种情况；
  - `from` 为 T、且 T 里有只在本地的合并结果 blob 时，`range_files` 能正常完成；
  - 检查本地对象时不会触发按需下载（结束后对象数量不变）。

**前端（`pnpm test`）：**
- 线程 → view zone 的放置（纯函数：输入线程列表、视图模式和 `getLineChanges()` 的结果，输出每侧的 zone、要强制展开的行和放进折叠区的线程）；`origToMod` 的换算。
- 已看状态的乐观更新和回滚、"下一个未看文件"的查找。
- 提交范围选择（单击、Shift+点击、根提交）到 `from`/`to` 的换算；切换文件时保留范围参数。
- DOMPurify 配置：脚本、事件属性、`style`、`javascript:` 和相对链接被去掉，链接被加上 `rel`。

**手动验收（真实 PR）：**
- 一个有已解决和过时评论的 PR：评论位置和 GitHub 一致，折叠区里的评论行被展开。
- 在本工具里标记已看后，GitHub 网页刷新能看到；反过来也一样。作者 push 之后再标记，得到 409 提示。
- 一个 rebase 过的 PR："上次 review 之后"只显示作者自己的改动。

## 实施顺序

每一步都能单独提交、单独验证：

1. 阶段 0 的技术验证，把结论写回 design.md §10。
2. 本地状态库；把 `load()` 拆出通用的 `range_files`（行为不变，M0 测试全部通过）；`fileQuery` 保留范围参数；开发代理改写 `Origin`。
3. 已看：后端查询和 PUT、前端勾选框 / 进度 / `v` / toast。
4. 提交列表和范围视图：后端 `/commits`、`ensure_commits`、`from`/`to`；前端提交选择器和 URL。
5. 上次 review 之后：`replay`、since-review；前端选择器第一项和横幅。
6. 评论线程：后端 `/threads`；前端 view zone、折叠区、文件树评论数、DOMPurify、CSP 响应头。
7. 收尾：mock、README（功能列表和路线图）、design.md 的更新；版本号改为 0.2.0。
