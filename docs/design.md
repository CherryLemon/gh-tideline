# Tideline 设计文档

- 状态：草案
- 日期：2026-10-12

## 1. 背景与目标

在 GitHub 网页上看 PR diff，有这些问题：

- 只显示改动片段（hunk）。想看上下文，只能一次次点"展开"。
- 大 PR 会卡，大文件默认被折叠。
- 左右两栏不同步，键盘操作很弱。
- 作者 force-push 之后，很难只看"上次 review 之后改了什么"。

JetBrains 自带的 PR 功能体验好，但要先 clone 仓库、打开工程、等索引建完，太重。

**一句话目标：不用 clone，也不用打开 IDE。给一个 PR 链接，几秒内就能看到 IDE 级别的 diff。**

产品形式：一个本地命令行程序（Rust），启动一个本地 HTTP 服务，在浏览器里打开网页界面（TypeScript）。
服务端负责 git 操作、调用 GitHub API 和建符号索引；浏览器负责展示和交互。

## 2. 范围

### 里程碑

| 里程碑 | 内容 |
|---|---|
| **M0 看 diff** | 输入 PR 链接后：变更文件树；整文件 diff（左右并排 / 单栏）；折叠未改动部分；语法高亮；词级高亮；忽略首尾空白；F7 跨文件跳到下一处改动；生成文件默认折叠；图片左右对比 |
| **M1 review 上下文** | 已看状态和 GitHub 双向同步；只读显示已有评论线程；按单个提交或提交范围查看；"上次 review 之后改了什么"（能处理 rebase） |
| **M2 代码跳转** | tree-sitter 符号索引；跳到定义（Ctrl+B / Ctrl+点击）；按符号名查找用法（Alt+F7）；顶部固定显示当前函数或类；文件大纲 |

### 不做的事（M0–M2 期间）

- 不做写操作：行内评论、修改建议、提交 review、approve。都回到 GitHub 网页上做。
- 不做 JetBrains 风格的曲线连接带。Monaco 的做法是插空白行让两边对齐，这一阶段先接受。
- 不接 LSP，也不做三方合并视图。
- 只支持 GitHub 和 GitHub Enterprise，不支持其他平台。但所有 GitHub 调用都集中在一个模块里，以后要扩展不会太难。
- 不做 AI 摘要。

## 3. 用户体验

### 3.1 入口

```bash
gh tideline https://github.com/owner/repo/pull/123   # 作为 gh 扩展
gh tideline owner/repo#123
gh tideline 123                                      # 在仓库目录里，从 origin 推断是哪个 repo
gh tideline                                          # 打开首页：最近看过的 PR + 输入框
```

- **本地 URL 和 GitHub 路径一一对应：** `http://localhost:<PORT>/github.com/owner/repo/pull/123`。
  只要把 GitHub 链接里的 `https://` 换成 `http://localhost:<PORT>/` 就能打开。
  所以浏览器扩展或书签脚本只要一行就能实现"在 tideline 中打开"：

  ```js
  javascript:location.href='http://localhost:7457/'+location.host+location.pathname
  ```

- **复用已经在跑的服务。** 如果服务已经在运行，命令行只负责打开浏览器。
  判断方法：读 `server.json`，再做一次健康检查。

### 3.2 界面布局

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

**变更文件树：**
- 按目录分组。用颜色和字母表示状态：A 新增、M 修改、D 删除、R 重命名。
- 每个文件显示增删行数、已看勾选、评论数。
- 已看状态是 DISMISSED 的文件（看过之后又被改了）用醒目的颜色标出来。
- 生成文件和 vendored 文件单独放一组，默认折叠。

**一次只显示一个文件。** 这和 JetBrains 一样，和 GitHub 把所有文件拼成一长条不一样。
好处是编辑器只需要一个实例，大 PR 的性能问题基本就没了。

### 3.3 快捷键

默认尽量贴近 JetBrains，并且可以配置。

| 键 | 功能 |
|---|---|
| F7 / Shift+F7 | 下一处 / 上一处改动。到文件末尾时自动进入下一个文件。Monaco 默认把这两个键绑给无障碍 diff 查看器，需要覆盖掉 |
| Alt+↓ / Alt+↑ | 下一个 / 上一个文件。不用 Alt+← / Alt+→，因为那是浏览器的后退和前进，下面要用到 |
| 连按两次 Shift、Ctrl+P | 按文件名查找并跳转 |
| v | 切换当前文件的已看状态；标记为已看时同时跳到下一个未看文件 |
| Ctrl+F | 在当前文件里搜索（Monaco 自带） |
| Ctrl+B / Ctrl+点击（M2） | 跳到定义 |
| Alt+F7（M2） | 查找用法。Linux 上 GNOME 和 KDE 会抢走这个键（用来移动窗口），所以同时提供 Ctrl+Alt+F7 |

**注意：**
- 浏览器自己占用的快捷键拦截不了，比如 Ctrl+T、Ctrl+W、Ctrl+N、Ctrl+Shift+N。
  所以 JetBrains 的 Ctrl+Shift+N（按文件名查找）改成连按两次 Shift 或 Ctrl+P。
- "返回上一个位置"直接用浏览器的历史记录：每次跳转都 `pushState`，浏览器的前进 / 后退就能用。

## 4. 架构

### 4.1 总览

```
 gh tideline <pr>  ──启动 / 复用──▶  本地服务 (Rust, 127.0.0.1:PORT)
                                ├─ git 层 ──── 调用 git 命令 ──▶ 缓存仓库 (bare + partial + shallow)
                                ├─ GitHub 层 ─ GraphQL / REST ──▶ api.github.com / GHE
                                ├─ 索引层 (M2) ─ tree-sitter-tags ──▶ SQLite
                                └─ HTTP: /api/*、/api/events (SSE)、静态资源 (嵌入在可执行文件里)
                                          ▲
 浏览器单页应用 (TS + React) ─────────────┘
   变更文件树 | Monaco DiffEditor (只有一个实例) | 评论 view zone | 状态栏
```

### 4.2 技术栈

| 层 | 选择 | 理由 |
|---|---|---|
| HTTP | axum + tokio | 生态主流 |
| 命令行参数 | clap | |
| GitHub | reqwest (rustls) + 手写 GraphQL 查询 + serde 类型 | 用到的接口很少，没必要引入 octocrab |
| git | 用 `tokio::process` 调用 `git` 命令；每个仓库常驻一个 `git cat-file --batch` 进程读 blob | 只有 git 命令完整支持部分克隆、按 OID 抓取、`merge-tree --merge-base`。gix 和 git2 都不支持按需从远端拉对象 |
| 嵌入前端 | rust-embed | 最后只有一个可执行文件 |
| 缓存目录 | directories | 遵守 XDG 规范 |
| 打开浏览器 | webbrowser | |
| 索引（M2） | tree-sitter + tree-sitter-tags + 各语言 grammar crate；用 rusqlite 存储 | tags.scm 正是 GitHub "按名字搜索的代码跳转"所用的那一套 |
| 前端 | Vite + React + TS | |
| 编辑器 | monaco-editor（打包进前端，不从 CDN 加载）+ @monaco-editor/react | 自带左右并排 / 单栏视图、折叠未改动部分、词级高亮、view zones，还能注册定义提供器 |
| 高亮 | shiki + @shikijs/monaco | 用 TextMate 语法，高亮质量和 VS Code 一样，比 Monaco 自带的 Monarch 好 |
| 列表 | @tanstack/react-virtual | 文件树要支持 3000 个文件 |
| 请求缓存 | @tanstack/react-query | |

**git 版本要求：至少 2.40**，因为要用 `merge-tree --merge-base` 和 `check-attr --source`。
本机是 2.43。启动时检查版本，太低就报错并给出提示。

### 4.3 Rust 模块

先只建一个 crate，下面每项是一个模块。以后变大了再拆成 workspace。

```
src/
  main.rs        命令行：解析输入、启动 / 复用服务、打开浏览器
  server/        axum 路由、安全中间件、静态资源、SSE
  git/           缓存仓库管理、fetch、diff、cat-file 进程池、每个仓库一把锁
  github/        token 获取、GraphQL / REST 客户端、ETag 缓存
  pr/            把 git 和 GitHub 拼起来：打开 PR、文件列表、提交、上次 review 之后的改动
  index/  (M2)   tree-sitter-tags 提取符号、SQLite、查定义和用法
  config.rs      配置文件 ~/.config/tideline/config.toml
web/             前端工程，构建产物会被 rust-embed 嵌入
```

### 4.4 缓存目录

```
$XDG_CACHE_HOME/tideline/
  server.json                          正在运行的服务：pid、端口、版本
  repos/<host>/<owner>/<repo>.git      bare 仓库，部分克隆 + shallow
  state.sqlite                         本地状态：最近打开的 PR、每个 PR 上次在本工具里看的 head
  index.sqlite             (M2)        符号索引，按 blob OID 存
```

**缓存仓库的维护：**
- 每个抓下来的提交都写一个 ref：`refs/dr/<pr>/<sha>`。
  有了 ref，`gh tideline gc` 才能按 PR 淘汰缓存；"上次 review 时的 head"也靠它保住，不会被回收。
- 设置 `gc.auto=0`，什么时候 gc 由本工具自己控制。

## 5. 数据流

### 5.1 打开 PR（M0）

```
1. 解析输入，得到 (host, owner, repo, number)

2. 一次 GraphQL 查询拿到：
   - PR 的 id、标题、作者、状态、baseRefName / baseRefOid、headRefName / headRefOid
   - 当前登录用户
   - files { path additions deletions changeType }：按 100 个一页翻页
     有了它，文件树在抓取任何 blob 之前就能显示增删行数
   (M1 起，同一次查询里再带上 files 的 viewerViewedState)

3. 算 merge-base，和 GitHub 的三点 diff 保持一致：
   REST GET /repos/{o}/{r}/compare/{baseRefOid}...{headRefOid}?per_page=1，取 merge_base_commit.sha
   (带上 per_page=1；不带的话会顺带返回最多 300 个文件的 patch，大 PR 很重)
   (失败时的兜底：加深 shallow 历史，再在本地跑 git merge-base)

4. 浅抓取两个提交，只要提交和目录树：
   git fetch --depth=1 --filter=blob:none origin <merge_base> <head>

5. 先拿文件列表。这一步不需要 blob：
   git diff-tree -r --raw -z --no-renames --no-abbrev <merge_base> <head>
   (要用底层命令 diff-tree 加 --no-abbrev：git diff --raw 即使加了 -z 也会输出缩写的 OID，
    缩写 OID 没法拿去抓取；底层命令也不受用户 diff.renames 之类配置的影响)

6. 一次性批量抓取所有改动文件的 blob，以及 head 版本里的 .gitattributes：
   git -c fetch.negotiationAlgorithm=noop fetch origin --no-tags --no-write-fetch-head \
       --recurse-submodules=no --filter=blob:none --stdin   <<< 完整 40 位 OID 列表
   (已实测：这正是 git 处理部分克隆缺失对象时内部运行的命令，在 GitHub 上可用)

7. 有了 blob，就可以在本地算重命名和准确的增删行数：
   git diff-tree -r --raw -z -M --numstat --no-abbrev <merge_base> <head>
   (这条命令即使没有第 6 步，自己也会批量预取一次；第 6 步的作用是把抓取时机掌握在自己手里)

8. 判断哪些是生成文件：
   git check-attr --source=<head> -z --stdin linguist-generated linguist-vendored
   再加上内置规则：*.lock、package-lock.json、pnpm-lock.yaml、go.sum、*.min.js 等

9. 返回 PR 信息和文件列表，前端渲染文件树，打开第一个文件
```

- **体验目标：** 第 5 步完成就把文件树推给前端，先显示出来，增删行数先用 GraphQL 给的值。
  第 6–8 步完成后，再通过 SSE 推送补全的重命名、增删行数和生成文件标记。
- **为什么要把 .gitattributes 放进第 6 步：** `check-attr --source` 遇到缺失的 .gitattributes 会单独再去抓一次。
- **同一个仓库的 fetch 必须串行。** 同时往一个仓库抓取会在 `shallow.lock` 上冲突，所以每个仓库一把 tokio Mutex。
- **fork 来的 PR 不用特殊处理。** head 提交在 base 仓库的 `refs/pull/N/head` 下面也能取到，所以按 SHA 抓取同样有效。

### 5.2 文件内容和 diff 计算

**服务端只提供 blob 的原始字节，diff 由 Monaco 在自己的 Worker 里算**（`diffAlgorithm: 'advanced'`，和 VS Code 用的是同一套算法）。

**前端怎么加载文件：**
- 打开文件时并发请求新旧两个 blob。
- 每个 blob 建一个 Monaco model，URI 是 `dr://<side>/<oid>/<path>`，语言按扩展名判断。
  URI 里要带上是哪一侧（side）：只改了文件权限时（状态 T），两侧的 OID 和路径完全一样，不区分的话 `createModel` 会报 "model already exists"。
- model 用 LRU 管理，最多大约 50 个，超出的 dispose。
- 打开一个文件时，预取下一个文件的 blob。
- 每个文件单独保存和恢复视图状态（`saveViewState` / `restoreViewState`），切回来时停在原来的位置。

**大文件降级：**

| 条件 | 处理 |
|---|---|
| 大于 1 MB | 关闭语法高亮 |
| 大于 5 MB，或超过 5 万行 | 不自动加载，显示"点击加载" |
| 最长一行超过 1 万字符（压缩过的文件） | 关闭高亮和词级 diff |

Monaco 自己也有两个上限要和上表对齐：`maxComputationTime`（默认 5000 毫秒）和 `maxFileSize`（默认 50 MB）。
diff 计算超时后结果会很难看，所以超过上表阈值的文件根本不交给 Monaco 算。

**换行符和编码：**
- Monaco 会把每个 model 统一成一种换行符。如果只是 CRLF 改成了 LF，Monaco 会显示"没有改动"，而 numstat 却算每一行都改了。
- 所以由服务端检测：换行符变化、BOM、UTF-16 或其他非 UTF-8 编码。检测到就在文件顶部显示一条提示。

**二进制文件：**
- numstat 里显示为 `-	-` 的就是二进制文件。
- 图片：前端 `fetch` 字节后生成 `blob:` URL，用 `<img>` 左右对比。不直接把 blob 接口地址当 img src，原因见第 8 节。
- 其他二进制文件：只显示"二进制文件，大小 a → b"。

**忽略空白：** Monaco 只支持 `ignoreTrimWhitespace`（忽略首尾空白），所以 M0 只提供"不忽略 / 忽略首尾空白"两档。
要做"忽略全部空白"，就得自己提供 diff 计算，放到以后。

**文件树里的增删行数**来自 git numstat，和 Monaco 视图里看到的可能有细微差别。这可以接受。

### 5.3 已看状态同步（M1）

- **读：** `pullRequest.files(first:100, after:$cursor) { nodes { path viewerViewedState } }`。状态有三种：VIEWED、UNVIEWED、DISMISSED。
- **写：** `markFileAsViewed(input:{pullRequestId, path})`，取消用 `unmarkFileAsViewed`。重命名的文件用新路径。
- **前端先更新界面再发请求。** 请求失败就回滚，并弹提示。
- **记录本地 head：** 用户在本工具里把文件标记为已看时，同时在 `state.sqlite` 里记下当前的 head SHA，给 5.6 当兜底基线用。

### 5.4 评论线程展示（M1，只读）

- **查询：**
  `reviewThreads(first:100) { nodes { id isResolved isOutdated path subjectType line startLine diffSide comments(first:50) { nodes { author{login} bodyHTML createdAt url } } } }`
  线程和评论两层都要翻页。
- **什么时候显示：** 只在查看完整 PR 范围（merge-base → head）时显示。
  - 没有过时的线程：根据 `diffSide`（LEFT 是旧文件，RIGHT 是新文件）和 `line`，作为 view zone 插在对应行下面。
  - 过时的线程和已解决的线程：放在侧栏列表里，默认折叠。
  - 文件级评论（`subjectType: FILE`）没有行号，即使没过时 `line` 也是 null。显示在文件顶部。
  - 有评论的行如果落在被 `hideUnchangedRegions` 折叠的区域里，view zone 会看不见。这些行必须强制展开。
- **行号可以直接用。** GitHub 的 `line` 是整个文件里的行号，正好对应我们的整文件 model，不需要换算。
- **安全：** `bodyHTML` 虽然是 GitHub 渲染好的，还是要再用 DOMPurify 清理一遍。

### 5.5 按提交或范围查看（M1）

- **拿提交列表：** GraphQL `commits(first:100) { nodes { commit { oid messageHeadline authoredDate author{name} parents(first:2){nodes{oid}} } } }`。
  GitHub REST 的 PR 提交列表最多返回 250 条；GraphQL 的 `commits` 有没有同样的上限要实测（见 S7）。
- **抓取：** 把 PR 的所有提交和它们的父提交按 SHA 一次性浅抓取（depth=1，blob:none）。
  完全不需要遍历历史，因为任何一段范围的 diff 都只是两棵目录树之间的比较。
- **选择器：**
  - 单个提交：父提交 → 该提交。merge 提交用第一个父提交。
  - 一段范围 [A..B]：A 的父提交 → B。
  - 状态写在 URL 上：`?from=<sha>&to=<sha>`。

### 5.6 上次 review 之后改了什么（M1）

**这是和 GitHub 网页拉开差距的关键功能。**

**1. 确定基线（旧的 head，记作 H_old）：** 按下面的优先顺序取。
  1. 当前用户在这个 PR 上最近一次 review 的 `commit.oid`。
     查询：`reviews(last:1, author:$viewerLogin, states:[APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED])`。
     必须带 `states`：不带的话可能取到用户还没提交的 PENDING review，它就在当前 head 上，得到的 diff 是空的。
  2. 本工具在 `state.sqlite` 里记录的、上次看到的 head。

**2. 抓取 H_old，并算出它的 merge-base（记作 M_old）。**
- **M_old 的算法：** compare `{当前 baseRefOid}...{H_old}`。
  - 如果 PR 中途改过目标分支，或者 base 分支自己被 force-push 过，这样算出来的值不可靠。这时直接走"冲突"那一行的退回方案。
- **抓取 H_old：** 已实测，force-push 之前的提交仍然能按 SHA 抓到，前提是 GitHub 还没把它当垃圾回收掉。
- **抓不到时：** 只能用本地缓存（以前看过的版本有 ref 保护，见 4.4）。
  缓存里也没有，就提示"上次 review 的版本已不可用"。
  不用 REST `git/trees` / `git/blobs` 做兜底：它们读的是同一个对象库，git 抓不到时它们也会 404。

**3. 计算要展示的 diff：**

| 情况 | 做法 |
|---|---|
| `M_old == M_new`（没有 rebase） | 直接比较 H_old → H_new |
| 发生了 rebase | 先把旧版 PR 重放到新的 base 上：`T = git merge-tree --write-tree --merge-base=M_old M_new H_old`。不冲突时展示 T → H_new，这样只剩作者自己的改动，不会混进 base 分支的变化 |
| 重放时有冲突 | 退回到 H_old → H_new，并在顶部提示"包含 base 分支的变化" |

**4. 界面：** 在提交选择器里加一项 "Since your last review (abc1234)"。

### 5.7 符号索引和代码跳转（M2）

**什么时候建索引：** 用户第一次请求跳到定义时，或者在设置里打开"自动建索引"后，在后台建，进度通过 SSE 推给前端。

**建索引的步骤：**
1. `git ls-tree -r -z --long <head>`，列出 head 版本的所有文件。
2. 按扩展名筛出支持的语言，并且只要小于等于 1 MB 的文件。
3. 用 5.1 第 6 步的方法，一次性批量抓取这些 blob。
4. 通过 `cat-file --batch` 逐个读出来，用 tree-sitter-tags 提取符号。

**索引按 blob OID 存，不按提交存。** 内容一样的文件 OID 也一样。
所以同一个仓库换一个 head、换一个 PR，只需要处理新出现的 blob，增量很小。

**表结构：**
```sql
tags(blob_oid, name, kind, is_definition, start_line, start_col, end_line, end_col)
tree_entries(commit_oid, path, blob_oid)
```

**用在哪里：**

| 功能 | 做法 |
|---|---|
| 文件大纲 / 顶部固定显示当前函数 | 把这个文件的定义类 tags 转成 Monaco 的 `DocumentSymbolProvider` |
| 跳到定义 | 注册 Monaco 的 `DefinitionProvider`，向服务端按 `name` 和语言查定义。排序：同一文件 > 同一目录 > 其他。目标文件在这个 PR 里，就打开它的 diff 并定位到行；不在，就用只读的普通编辑器打开 head 版本 |
| 查找用法 | 查 `is_definition = false` 的同名 tags（tags.scm 里的 `@reference.*`），在侧栏列出。结果是近似的，界面上要写明"按名称匹配" |

**范围：**
- 默认只给 head 版本建索引。在旧文件（左侧）上请求跳转时，再按需给 merge-base 版本建索引。
- 第一批支持的语言：Rust、Go、Python、TypeScript/TSX、JavaScript、Java、C、C++。这八个 grammar 都带 tags.scm，Rust crate 也都导出了 `TAGS_QUERY`。
  - TypeScript 的 tags.scm 只覆盖 TS 特有的语法，要和 JavaScript 的 tags.scm 拼在一起用。
  - C 和 C++ 的 tags.scm 只有定义，没有引用。查找用法要自己写 reference 查询，在那之前对 C/C++ 返回空。
- tree-sitter 给出的列号是字节偏移，Monaco 要的是 UTF-16 列号，需要转换。

## 6. HTTP API

所有接口都以 `/api/{host}/{owner}/{repo}` 开头，下表省略这一段。

| 方法 | 路径 | 说明 | 里程碑 |
|---|---|---|---|
| GET | `/pulls/{n}` | PR 信息：标题、作者、状态、base 和 head、merge-base、节点 id | M0 |
| GET | `/pulls/{n}/files?from=&to=` | 文件列表，结构见下方 `ChangedFile`；不带参数时就是完整 PR 范围 | M0 |
| GET | `/blobs/{oid}` | 文件原始字节，带 `Cache-Control: immutable` | M0 |
| GET | `/pulls/{n}/commits` | 提交列表 | M1 |
| GET | `/pulls/{n}/threads` | 评论线程 | M1 |
| PUT | `/pulls/{n}/viewed` | 请求体 `{path, viewed}` | M1 |
| GET | `/pulls/{n}/since-review` | 返回 `{from, to, mode: direct \| rebased \| fallback, baseline}` | M1 |
| GET | `/revs/{sha}/symbols?path=` | 一个文件的符号 | M2 |
| GET | `/revs/{sha}/definitions?name=&lang=` | 查定义 | M2 |
| GET | `/revs/{sha}/references?name=&lang=` | 查用法 | M2 |
| GET | `/api/events` | SSE 推送：抓取进度、索引进度、文件列表补全 | M0 |

```ts
interface ChangedFile {
  path: string;
  oldPath?: string;                       // 重命名或复制时才有
  status: "A" | "M" | "D" | "R" | "C" | "T";
  oldOid: string | null;
  newOid: string | null;
  oldMode: string;
  newMode: string;
  additions?: number;                     // numstat 补全之前没有
  deletions?: number;
  binary: boolean;
  generated: boolean;
  viewed?: "VIEWED" | "UNVIEWED" | "DISMISSED";
}
```

## 7. 前端要点

**路由：**
- `/`：最近看过的 PR，加一个输入框。
- `/{host}/{owner}/{repo}/pull/{n}?file=&from=&to=`：查看某个 PR。

**DiffEditor 配置：**

```ts
{
  readOnly: true,
  renderSideBySide: true,
  hideUnchangedRegions: { enabled: true },
  ignoreTrimWhitespace: false,   // 默认是 true，必须显式关掉
  diffAlgorithm: "advanced",
  renderOverviewRuler: true,
  experimental: { showMoves: true },   // 移动检测
}
```

**Monaco 的加载：**
- 用 `loader.config({ monaco })`，改为加载打包进来的 monaco-editor。
  `@monaco-editor/react` 默认从 jsdelivr CDN 加载。
- 在 Vite 里还要设置 `self.MonacoEnvironment.getWorker`。
  否则 diff 计算会退回主线程，大文件会卡住界面。

**F7 跳转：**
- 先覆盖 Monaco 自带的 F7 / Shift+F7 绑定（默认是 `AccessibleDiffViewerNext/Prev`）。
- 在 `onDidUpdateDiff` 之后读 `getLineChanges()`，自己实现跳转。
- 已经是最后一处改动时，打开下一个文件，等它的 diff 算完，再定位到第一处改动。

**状态管理：** 用 React Query 缓存服务端数据。界面状态（当前文件、视图模式、忽略空白）保存在 URL 和 localStorage 里。

## 8. 安全

本地服务手里有 GitHub token，而且有写接口（标记已看），所以要防止其他网页借用户的浏览器访问它。

- **只监听 127.0.0.1。**
- **路径里的 host 必须在白名单里（最关键的一条）：**
  - 只允许 github.com 和配置文件里显式列出的 GHE 域名。
  - 否则，任何网页放一个 `<img src="http://localhost:PORT/api/evil.com/...">`，服务端就会带着用户的 token 去请求 evil.com：API 请求和 git 的 http.extraHeader 都会带上 token。
  - 同样的问题还能被用来做 SSRF，或者让服务端抓取任意仓库把磁盘写满。
- **token 按 host 选取，规则和 gh 一致：**
  - github.com：用 `GH_TOKEN`、`GITHUB_TOKEN`。
  - GHE：用 `GH_ENTERPRISE_TOKEN`、`GITHUB_ENTERPRISE_TOKEN`。
  - 都没有时用 `gh auth token --hostname <host>`。
- **会触发抓取的 GET 请求也必须同源：** 校验 `Sec-Fetch-Site: same-origin`，或者带 `X-DR` 头。
- **本机密钥：**
  - 本机上其他进程、其他用户也能连 127.0.0.1:PORT，所以 `/api/*` 需要一个每次安装生成的密钥。
  - 命令行打开浏览器时用一次性 URL 把密钥写进 HttpOnly、SameSite=Strict 的 cookie。
  - 书签或浏览器扩展打开的页面如果还没有这个 cookie，就提示用户先运行一次 `gh tideline`。
- **校验 Host 头：** 只接受 `localhost:PORT` 和 `127.0.0.1:PORT`，防止 DNS rebinding。
- **不返回任何 CORS 头：** 其他网站发来的请求读不到响应内容。
- **非 GET 请求做两项检查：**
  - `Origin` 必须和本服务一致；
  - 必须带自定义头 `X-DR: 1`。跨域请求带自定义头会先发预检请求，服务端拒绝预检即可。
- **token 只留在服务端，不下发给前端。**
  - 给 git 用时，通过环境变量 `GIT_CONFIG_COUNT/KEY/VALUE` 注入 `http.extraHeader`。
  - 不写进命令行参数，因为 `ps` 能看到命令行参数。
- **`/blobs/{oid}` 的响应头一律设为：**
  - `Content-Type: application/octet-stream`
  - `X-Content-Type-Options: nosniff`
  - `Content-Security-Policy: sandbox`

  否则，PR 里一个恶意 SVG 被直接打开时，会在本服务的源下执行脚本，然后以同源身份调用写接口。
- **评论 `bodyHTML` 用 DOMPurify 清理后再显示。**
- **拿不到 token 时：** 报错，提示用户执行 `gh auth login`。

## 9. 性能目标

都是初定数字，要在技术验证阶段实测。

| 场景 | 目标 |
|---|---|
| 中型 PR（不超过 100 个文件），仓库已有缓存：从执行命令到看到文件树 | 小于 1 秒 |
| 同上，大仓库第一次打开 | 小于 5 秒（和网络有关，见 S1） |
| 切换文件（blob 已缓存） | 小于 100 毫秒 |
| 3000 个文件的 PR，滚动文件树 | 流畅不掉帧 |
| 服务常驻内存（不含索引） | 小于 100 MB |

## 10. 风险和技术验证清单

开始写 M0 之前，先逐项做最小验证。

审查时已经在 cli/cli 仓库上实测过以下几点：
- 按 SHA 浅抓取提交：1.7 秒；
- `fetch --stdin` 批量抓取 21 个 blob：1.2 秒；
- force-push 之前的旧提交仍能按 SHA 抓到；
- `merge-tree` 和 `diff -M` 在 shallow + partial 仓库里都只批量预取一次，不会逐个懒加载。

| 编号 | 要验证什么 | 怎么验证 | 不行的话怎么办 |
|---|---|---|---|
| S1 | 功能已确认可用，还剩两点：① 在大仓库上的速度；② git over HTTPS 的 `http.extraHeader` 用 `Bearer <token>` 还是 `Basic base64(x-access-token:<token>)` | 拿 microsoft/vscode、torvalds/linux 的真实 PR 测从零开始到看到文件树的时间；两种认证头各试一次 | 改用完整历史的 blob:none 部分克隆 |
| S2 | 已确认可用，只要 GitHub 还没回收。剩下要验证的是：旧提交被回收后的错误表现 | 找一个很久以前被 force-push 过的 PR 试 | 只能用本地缓存，否则提示不可用 |
| S3 | Monaco 单实例加 model LRU，在 1000 个文件的 PR 上内存占用和切换耗时如何；`hideUnchangedRegions` 好不好用 | 写一个原型页面测 | 调整 LRU 大小，或者换 CodeMirror 6 的 merge 视图 |
| S4 | 已确认：能正常工作，只批量预取一次。不用再验证 | — | — |
| S5 | 3000 个文件的 PR，GraphQL 分页（30 页）会消耗多少速率限额 | 看响应里的 rateLimit 字段 | 用 ETag 做条件请求，并缓存结果 |
| S6（M2） | DiffEditor 里的 sticky scroll 会不会用我们自定义的 DocumentSymbolProvider | 写原型测 | 自己画一个顶部的函数或类名提示条 |
| S7 | GraphQL 的 `commits` 有没有 250 条的上限 | 找一个超过 250 个提交的 PR 测 | 超过上限时在界面上提示 |

## 11. 待定问题

- **可执行文件名：** 暂定 `gh-tideline`，这样既能作为 gh 扩展用 `gh tideline` 调用，也能单独运行。
- **默认端口：** 浏览器扩展和书签需要固定端口。暂定一个可以配置的固定端口，比如 7457。
- **服务怎么运行：** 空闲 N 分钟后自动退出，还是作为 systemd 用户服务一直运行？前者开销小，后者让浏览器扩展随时可用。
- **缓存回收：** 提供 `gh tideline gc` 命令，按缓存大小淘汰最久没用的。但"上次 review 时的 head"要保留。
- **曲线连接带：** M2 之后要不要在 Monaco 上面自己画一层，做出 JetBrains 风格的曲线连接带。
