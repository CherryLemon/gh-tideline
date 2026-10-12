# M0 骨架：实现约定

本文档是 `docs/design.md` 中 M0 的落地范围，也是后端（Rust）和前端（`web/`）之间的接口契约。两边都以本文为准。

## 范围

**做：**
- 命令行：解析输入、复用已经在跑的服务或启动新服务、打开浏览器。
- 服务端：安全中间件、打开 PR 的完整流程（design.md §5.1 第 2–8 步，同步执行）、读取 blob、托管静态资源。
- 前端：首页、PR 页（变更文件树 + 单实例 Monaco DiffEditor）、按文件名查找、F7 跨文件跳转、大文件和二进制文件降级、图片左右对比、Sandy Beach 主题。

**这一版不做（后续里程碑再补，不留占位代码）：**
- SSE 推送：文件列表一次性同步返回。
- 常驻的 `cat-file --batch` 进程：每次请求单独启动 `git cat-file`。
- Shiki 高亮：先用 Monaco 自带的 Monarch。
- 换行符和编码检测。

## 命令行

```
gh-tideline [TARGET] [--port <PORT>] [--no-open]
```

**TARGET 支持的写法：**
- `https://github.com/o/r/pull/123`（可以带 `/files` 等后缀，也可以是 GHE 域名）
- `o/r#123`，host 默认是 github.com
- `123`：从当前目录的 `git remote get-url origin` 推断 host、owner、repo，ssh 和 https 两种地址都要支持
- 不给：打开首页

**行为：**
1. 读配置文件 `$XDG_CONFIG_HOME/tideline/config.toml`（可以不存在）：

   ```toml
   port = 7457
   ghe_hosts = []
   ```

2. 读取或生成本机密钥 `$XDG_CONFIG_HOME/tideline/secret`：32 字节随机数，hex 编码，文件权限 0600，目录权限 0700。
   - 生成时用 `O_CREAT|O_EXCL`（`create_new`）原子创建；两个 gh-tideline 同时生成时，后到的读回先到者写入的密钥。
   - 文件存在但内容不是 64 位 hex 时报错，提示用户删除后重试（不再静默覆盖）。
3. 请求 `GET http://127.0.0.1:PORT/api/health`。如果返回 `{"app":"tideline"}`，说明服务已在运行，只需要打开浏览器（第 5 步）然后退出。
   - 这个应答不可信（端口可能被别的进程抢占），所以**密钥本身永远不上网络**，命令行只发出第 5 步的一次性 token。
4. 否则在前台启动服务，监听 `127.0.0.1:PORT`。启动后写 `$XDG_CACHE_HOME/tideline/server-<PORT>.json`，内容是 `{pid, port, version}`；退出时只有当文件里的 pid 仍是自己时才删除。
5. 打开浏览器的步骤：
   1. 命令行在本地生成一次性 token：`<ts>.<rand>.<mac>`
      - `ts`：当前 Unix 秒数（十进制）；`rand`：16 字节随机数的小写 hex（32 个字符）；
      - `mac = hex(HMAC_SHA256(secret, "dr-auth|" + PORT + "|" + ts + "|" + rand))`。
   2. 打开 `http://localhost:PORT/__auth?t=<token>&next=<path>`。
   3. 服务端校验后设置 cookie，再 302 跳到 `next`。`next` 必须以 `/` 开头，且不能以 `//` 开头。校验规则：
      - MAC 用常数时间比较；MAC 里绑定了端口，发给别的端口的 token 无效；
      - `ts` 距今不超过 60 秒，允许 5 秒的时钟超前；
      - `ts` 不早于服务启动时间：抢占端口的进程收到的 token，不能拿去给之后才启动的真服务用；
      - `rand` 只能用一次：用过的值记在内存里直到过期。

   cookie 名带端口：`dr_session_<PORT>`；值是 `/__auth` 当场生成的 32 字节随机数（hex），只存在服务端内存里，30 天过期。属性 `HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000`。
   - 服务重启后所有会话失效，401 提示用户运行 `gh-tideline` 重新授权。

## 安全中间件（design.md §8）

- **Host 头：** 必须是 `localhost:PORT` 或 `127.0.0.1:PORT`，否则返回 421。
- **`/api/*`：** 除了 `/api/health`，都必须同时满足：
  - 有合法的 `dr_session_<PORT>` cookie：请求里同名 cookie 可能有多个（别的页面可以往 `localhost` 塞同名 cookie），任意一个是有效会话即可；比较用常数时间；
  - 并且 `Sec-Fetch-Site` 是 `same-origin`，或者带有 `X-DR: 1` 头。
- **非 GET 请求：** `Origin` 必须是 `http://localhost:PORT` 或 `http://127.0.0.1:PORT`，并且带 `X-DR: 1`。
- **路径里的 `{host}`：** 必须是 `github.com` 或 `ghe_hosts` 里列出的域名，否则返回 403，错误码 `forbidden_host`。
- **不返回任何 CORS 头。**
- **token 按 host 选取：**
  - github.com：`GH_TOKEN` → `GITHUB_TOKEN` → `gh auth token --hostname github.com`
  - GHE：`GH_ENTERPRISE_TOKEN` → `GITHUB_ENTERPRISE_TOKEN` → `gh auth token --hostname <host>`
- **token 不下发给前端，也不出现在任何进程的命令行参数里。**

## git 层

**缓存仓库：**
- 位置：`$XDG_CACHE_HOME/tideline/repos/<host>/<owner>/<repo>.git`，用 `git init --bare --shared=0600` 创建（即 `core.sharedRepository=0600`：git 新建的目录 0700、文件 0600，与 umask 无关）。
- remote 是 `https://<host>/<owner>/<repo>.git`。
- 设置 `gc.auto=0`。
- **权限：** 配置目录、缓存目录、`repos/` 及其下的 host、owner、`<repo>.git` 目录一律 0700（用 `DirBuilder` + mode 创建）。服务启动时把已有的这些目录 chmod 成 0700；已有仓库在本进程第一次加锁时补设 `core.sharedRepository=0600`。

**每次调用 git 的环境：** 先清空环境变量，只透传白名单：`PATH`、`HOME`、`LANG`、`LC_*`（`LC_ALL` 和 `LC_MESSAGES` 除外）、`TMPDIR`、代理变量（`http_proxy`/`https_proxy`/`no_proxy`/`all_proxy` 的大小写两种形式）、`SSL_CERT_FILE`、`SSL_CERT_DIR`、`GIT_SSL_CAINFO`、`GIT_SSL_CAPATH`。再设置：
- `GIT_TERMINAL_PROMPT=0`；`SSH_ASKPASS`、`GIT_ASKPASS`、`DISPLAY` 等都不透传，所以不会弹出密码对话框
- `GIT_CONFIG_NOSYSTEM=1`
- `GIT_CONFIG_GLOBAL=/dev/null`：隔离用户的 insteadOf、credential helper 等全局配置
- `GIT_LITERAL_PATHSPECS=1`：来自 PR 内容的路径不会被当成 pathspec magic（例如名为 `:!x` 的目录）
- `LC_MESSAGES=C`：部分错误按 stderr 文本分类
- 通过 `GIT_CONFIG_COUNT/KEY_n/VALUE_n` 注入 `http.https://<host>/.extraHeader`，值为 `Authorization: Basic base64("x-access-token:<token>")`

来自外部（GitHub API）的 commit id 在解析后立刻校验，必须匹配 `^[0-9a-f]{40}$` 或 `^[0-9a-f]{64}$`；传给 git 的修订参数前加 `--end-of-options`。

**加锁：** 所有会写仓库的 git 命令都在"仓库锁"下执行：同进程内一把 tokio Mutex，再加上 `<repo>.git/gh-tideline-mutex` 上的排他 `flock`（多个 gh-tideline 服务共用缓存目录时互斥）。
- 拿到锁之后先删除残留的 git 锁文件（git 目录顶层的 `*.lock`，如 `shallow.lock`、`packed-refs.lock`、`HEAD.lock`、`config.lock`，以及 `refs/**/*.lock`）。这是安全的：每个 gh-tideline 服务都只在持有这把 flock 时运行会写仓库的 git，而且服务退出前会等自己的 git 子进程结束（最多 30 秒），所以此时看到的锁文件一定是被杀掉的 git 留下的。手动在缓存仓库里跑 git 不受支持。
- git 子进程不会因为 HTTP 请求断开而被杀掉：打开 PR 的流程在独立的 tokio 任务里跑，任务持有缓存槽和仓库锁，结果写回缓存槽，请求只是等待它。

**打开 PR 的流程：**
1. GraphQL：PR 信息，以及 `files { path additions deletions changeType }`（翻页）。
2. REST：`compare/{baseRefOid}...{headRefOid}?per_page=1`，取 `merge_base_commit.sha`。
3. `git fetch --depth=1 --filter=blob:none origin <mb>:refs/dr/pull/<n>/base <head>:refs/dr/pull/<n>/head`
   （如果按 SHA 写 ref 不行，就先 fetch，再用 `update-ref`）
4. `git diff-tree -r --raw -z --no-renames --no-abbrev <mb> <head>`，收集所有非零的 blob OID，再加上 head 中可能影响这些文件的 `.gitattributes` 的 OID：
   - 候选目录是根目录和 head 中存在的变更文件的各级父目录（它们在 head 里一定是 tree）；
   - 用 `git cat-file --batch` 从 stdin 读入 `<head>:<dir>`，解析 tree 内容，取名为 `.gitattributes` 且 mode 为 100644/100755 的条目。tree 在 blob:none 的部分克隆里都在本地，不会触发按需下载；用 stdin 也不受命令行长度限制。
   - （不用 `cat-file --batch-check` 查 `<head>:<dir>/.gitattributes`：git 2.43 上即使只要 `%(objectname)` 也会逐个按需下载 blob。）
5. `git -c fetch.negotiationAlgorithm=noop fetch origin --no-tags --no-write-fetch-head --recurse-submodules=no --filter=blob:none --stdin`，从 stdin 喂完整的 OID 列表。
6. `git diff-tree -r -z -M --raw --numstat --no-abbrev <mb> <head>`，解析出 `ChangedFile` 列表。
7. `git check-attr --source=<head> -z --stdin linguist-generated linguist-vendored`，再加上内置规则：
   `*.lock`、`package-lock.json`、`pnpm-lock.yaml`、`yarn.lock`、`go.sum`、`Cargo.lock`、`*.min.js`、`*.min.css`、`*.map`、`*.snap`。

**结果缓存：** 按 `(host, owner, repo, n)` 在内存里缓存 30 秒。带 `?refresh=1` 时，只复用"开始时间不早于本请求开始前 2 秒"的计算结果：前端一次刷新会并发发出 `/pulls/{n}` 和 `/pulls/{n}/files` 两个请求，它们共享同一次计算、同一份快照。

## HTTP API

所有 JSON 字段用 camelCase。

**出错时统一返回：**

```json
{ "error": { "code": "...", "message": "..." } }
```

| 错误码 | HTTP 状态 |
|---|---|
| `unauthorized` | 401 |
| `forbidden_host` | 403 |
| `not_found` | 404 |
| `no_token` | 500 |
| `github_error` | 502 |
| `git_error` | 502 |
| `bad_request` | 400 |

**接口：**

| 方法 | 路径 | 返回 |
|---|---|---|
| GET | `/api/health` | `{ "app": "tideline", "version": "0.1.0" }` |
| GET | `/__auth?t=&next=` | 校验一次性 token，设置 cookie 后 302 跳转 |
| GET | `/api/{host}/{owner}/{repo}/pulls/{n}` | `PrSummary` |
| GET | `/api/{host}/{owner}/{repo}/pulls/{n}/files` | `ChangedFile[]` |
| GET | `/api/{host}/{owner}/{repo}/blobs/{oid}` | 原始字节，响应头见下 |
| GET | 其他所有非 `/api` 路径 | 有对应静态文件就返回静态文件，否则返回 `index.html`（前端是单页应用） |

**blob 接口：**
- `oid` 必须匹配 `^[0-9a-f]{40}$` 或 `^[0-9a-f]{64}$`，否则返回 400。
- 先在本地读，不加锁、不按需下载（git 2.43 没有 `--no-lazy-fetch`，用 `-c protocol.allow=never cat-file --batch`：缺对象时 git 发起的 promisor fetch 立即失败，不碰仓库）。
- 本地没有时：
  - 只有本进程打开过的 PR 里列出的 blob 才去按需下载，在仓库锁下、独立任务里执行；下载不到（`not our ref`、`could not fetch … from promisor remote`）返回 404 `not_found`；
  - submodule 的 commit（gitlink）和没出现在任何已打开 PR 里的 OID 直接返回 404 `not_found`，不加锁、不访问网络。
- 对象不是 blob（tree、commit 等）时返回 404 `not_found`。
- 响应头：
  - `Content-Type: application/octet-stream`
  - `X-Content-Type-Options: nosniff`
  - `Content-Security-Policy: sandbox`
  - `Cache-Control: private, max-age=31536000, immutable`

```ts
interface PrSummary {
  host: string; owner: string; repo: string; number: number;
  title: string; author: string; state: "OPEN" | "CLOSED" | "MERGED"; url: string;
  base: { ref: string; sha: string };
  head: { ref: string; sha: string };
  mergeBase: string;
  nodeId: string;
}

interface ChangedFile {
  path: string;
  oldPath?: string;                                  // 只有 R / C 才有
  status: "A" | "M" | "D" | "R" | "C" | "T";
  oldOid: string | null;                             // 新增文件为 null（不是全 0）
  newOid: string | null;                             // 删除文件为 null
  oldMode: string;
  newMode: string;
  additions?: number;
  deletions?: number;
  binary: boolean;                                   // numstat 为 "-\t-"
  generated: boolean;
}
```

## 前端

**技术栈：** Vite + React + TS，pnpm。
- `monaco-editor` 打包进来，不从 CDN 加载：用 `loader.config({ monaco })`。
- worker 通过 Vite 的 `?worker` 导入，并设置 `self.MonacoEnvironment.getWorker`。
- 开发时 Vite 跑在 5173 端口，把 `/api` 和 `/__auth` 代理到 7457。
- 所有 fetch 都带 `X-DR: 1`。

**路由：**
- `/`：首页，一个输入框。粘贴 GitHub PR 链接或 `o/r#n`，跳到 PR 页。
- `/{host}/{owner}/{repo}/pull/{n}?file=<path>`：PR 页。

收到 401 时显示："请在终端运行 `gh-tideline` 授权此浏览器"。

**布局：**
- **顶栏：**
  - `owner/repo #n 标题`
  - `base ← head`
  - 左右并排 / 单栏切换
  - 忽略首尾空白开关
- **左侧变更文件树：**
  - 按目录分组，单一子目录链要压缩成一行（例如 `src/foo/bar`），和 JetBrains 一样。
  - 每个文件显示状态字母和颜色、文件名、`+a -d`。
  - 重命名的文件显示 `old → new`。
  - 生成文件放进单独的 "Generated" 组，默认折叠。
  - 文件很多时用 `@tanstack/react-virtual` 做虚拟列表。
  - 显示总文件数和总增删行数。
- **右侧：** 一个 DiffEditor 实例。
  - model 的 URI 是 `dr://{side}/{oid}/{path}`，用 LRU 管理，大约 50 个。
  - 每个文件单独保存和恢复视图状态。
  - 打开一个文件时预取下一个文件的 blob。
- **底部状态栏：**
  - 当前是第几处改动 / 共几处
  - 快捷键提示

**DiffEditor 配置：**

```ts
{
  readOnly: true,
  renderSideBySide: true,          // 可切换
  ignoreTrimWhitespace: false,     // 可切换
  hideUnchangedRegions: { enabled: true },
  diffAlgorithm: "advanced",
  experimental: { showMoves: true },
}
```

还要按大文件阈值设置 `maxComputationTime` 和 `maxFileSize`。

**快捷键：**

| 键 | 功能 |
|---|---|
| F7 / Shift+F7 | 下一处 / 上一处改动，到文件末尾时跨到下一个文件。必须覆盖 Monaco 自带的 F7 绑定 |
| Alt+↓ / Alt+↑ | 下一个 / 上一个文件 |
| 连按两次 Shift、Ctrl+P | 按文件名模糊查找并跳转 |

每切换一次文件就 `pushState`，让浏览器的后退可以用。

**降级规则：**

| 条件 | 处理 |
|---|---|
| 大于 1 MB | 关闭高亮（语言设为 plaintext） |
| 大于 5 MB，或超过 5 万行 | 显示"点击加载" |
| 最长一行超过 1 万字符 | 关闭高亮 |
| `binary: true` 且是位图（png/jpeg/gif/webp/avif/bmp/ico） | `fetch` 字节后生成 `blob:` URL，左右两张 `<img>` 对比 |
| `binary: true` 的 SVG | 先用 `data:` URL 载入 `<img>`，在 canvas 上转成 PNG，再生成 `blob:` URL。绝不生成 SVG 类型的同源 URL（见 design.md §8） |
| 任一侧 mode 为 `160000`（子模块） | 显示 `Subproject commit <old> → <new>`，不请求 `/blobs` |
| `binary: true` 的其他文件 | 显示"二进制文件" |

## 主题：Sandy Beach（Material Theme UI 官方配色）

**界面颜色：**

| 名称 | 值 |
|---|---|
| Background | `#FFF8ED` |
| Foreground | `#546E7A` |
| Text | `#888477` |
| Selection Background | `#E7C496` |
| Selection Foreground | `#FFFFFF` |
| Buttons | `#F6D7B0` |
| Second Background | `#F6EDDA` |
| Disabled | `#B8B6A5` |
| Contrast | `#FFF3DB` |
| Active | `#FEF8EC` |
| Border | `#F2EDDE` |
| Highlight | `#FBF1DF` |
| Tree | `#E7C49680` |
| Notifications | `#F6D7B0` |
| Accent | `#53C7F0` |
| Excluded | `#F2E7CA` |

**语法颜色：**

| 名称 | 值 |
|---|---|
| Comments | `#888477` |
| Variables | `#272727` |
| Links | `#39ADB5` |
| Functions | `#6182B8` |
| Keywords | `#7C4DFF` |
| Tags | `#E53935` |
| Strings | `#91B859` |
| Operators | `#39ADB5` |
| Attributes | `#F6A434` |
| Numbers | `#F76D47` |
| Parameters | `#F76D47` |

**文件状态颜色：**

| 状态 | 颜色 | 来源 |
|---|---|---|
| A 新增 | `#91B859` | Strings |
| M 修改 | `#6182B8` | Functions |
| D 删除 | `#B8B6A5` | Disabled，文件名加删除线 |
| R / C 重命名、复制 | `#39ADB5` | Links |
| T 类型变更 | `#F6A434` | Attributes |

**diff 颜色：**

| 用途 | 值 |
|---|---|
| 新增行背景 | `#91B85926` |
| 新增文字背景 | `#91B85966` |
| 删除行背景 | `#E5393520` |
| 删除文字背景 | `#E5393550` |
| 对角填充 | `#F2E7CA` |
| 折叠区域背景 | `#F6EDDA` |
| 折叠区域前景 | `#888477` |

**字体：**
- 代码用 JetBrains Mono，通过 `@fontsource/jetbrains-mono` 打包，离线可用。
- 界面文字用系统字体。

## 构建

- `make web`：在 `web/` 里执行 `pnpm install && pnpm build`。
- `make build`：先 `make web`，再 `cargo build --release`。前端产物通过 rust-embed 嵌入，目录是 `web/dist`。
- `make dev`：说明怎样同时跑 `cargo run` 和 `pnpm dev`。
