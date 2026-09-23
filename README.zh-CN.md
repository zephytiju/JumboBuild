# Jumbo Build

[English](./README.md)

Jumbo Build 是 Juntai 统一的项目构建与多仓库工作空间管理命令行工具。它为所有受支持的语言提供一致的构建、测试、格式化、发布检查和清理流程，同时通过轻量的插件接口封装各语言特有的行为。

## 为什么使用 Jumbo Build？

- **统一的项目工作流：** 在所有受支持的语言中使用同一组命令。
- **多仓库工作空间：** 在一个工作空间内克隆、导入、移除、同步和监听多个仓库。
- **本地依赖连接：** 将已检出的 Python 包作为 uv workspace 成员连接，本地缺失时可回退到已记录的 Git 远程地址；Node 项目同样以 npm 包身份注册在其中。
- **原生 CLI：** 为 macOS 和 Linux 提供单个 Rust 二进制文件。
- **可扩展的语言支持：** 实现并注册 `LanguageSupport` 即可接入新的生态系统。

Jumbo Build 目前支持通过 `pyproject.toml` 识别的 Python 项目，以及通过 `package.json` 识别的 Node.js / TypeScript 项目。

## 前置条件

- x86-64 或 ARM64 架构的 macOS / Linux
- Git
- Rust stable 与 Cargo（安装脚本会从源码构建 Jumbo Build）
- Python 项目需要 [uv](https://docs.astral.sh/uv/)；pytest、Ruff 等项目工具应声明在项目的依赖组中
- Node 项目需要 PATH 中的 Node.js ≥ 18 与 npm ≥ 9（lockfileVersion 2/3 的硬性下限是 npm ≥ 7）

## 安装

安装脚本会构建当前检出的源码，并将 `jumbo` 复制到 `~/.local/bin`：

```bash
git clone https://github.com/zephytiju/JumboBuild.git
cd JumboBuild
./install.sh
```

安装完成后打开新的 shell，并验证二进制文件：

```bash
jumbo --version
jumbo --help
```

## 快速开始

创建工作空间并添加仓库：

```bash
jumbo workspace create my-workspace
cd my-workspace
jumbo workspace use -r https://github.com/example/example-package.git
```

项目构建命令必须在已注册的项目目录内运行。Jumbo 会根据 `jumbo.toml` 识别当前项目，并且只操作该项目：

```bash
cd projects/example-package
jumbo          # 更新锁文件、同步并构建
jumbo test     # 构建并测试
jumbo format   # 构建、格式化并应用安全的 lint 修复
jumbo release  # 构建、测试并运行严格 lint 检查
jumbo clean    # 清理当前项目的生成文件
```

`jumbo release` 是发布就绪检查，不会实际发布构建产物。

## 命令参考

### 项目命令

| 命令 | Python 流程 | Node 流程 |
| --- | --- | --- |
| `jumbo` 或 `jumbo build` | `uv lock --upgrade` → `uv sync` → `uv build` | `npm install --package-lock-only --ignore-scripts` → `npm ci` → `npm run build`（配置了 `build` 脚本时） |
| `jumbo test` | 构建流程 → `uv run pytest -v` | 构建流程 → `npm test`（已配置时；npm 生成的 "no test specified" 占位脚本视为未配置）或 `node --test`（存在 Node 测试文件时） |
| `jumbo format` | 构建流程 → `ruff format .` → `ruff check --fix .` | 构建流程 → `npm run format`（已配置时）或 `npx --no-install prettier --write .`（已配置 Prettier 时） |
| `jumbo release` | 构建流程 → `uv run pytest -v` → `ruff check .` | 构建流程 → 测试步骤 → `npm run format:check`（已配置时）或 `npx --no-install prettier --check .` |
| `jumbo clean` | 删除当前项目的缓存、覆盖率结果和构建产物 | 删除 `node_modules/`、构建输出（`dist`、`build`、`out`）、覆盖率结果、`.eslintcache` 和 `*.tsbuildinfo` |

显式形式 `jumbo build test`、`jumbo build format`、`jumbo build release` 和 `jumbo build clean` 与对应的顶层快捷命令等价。

### 工作空间命令

除非命令另有说明，可在工作空间根目录的任意下级目录运行工作空间命令。`workspace` 可简写为 `ws`。

| 命令 | 用途 |
| --- | --- |
| `jumbo workspace create <name>` | 创建 `<name>/`、`projects/`、`jumbo.toml`、根 `pyproject.toml` 和 VS Code 工作空间配置 |
| `jumbo workspace create <name> --import` | 初始化已有的 `<name>/` 目录，并导入其 `projects/` 下的现有文件夹 |
| `jumbo workspace use -r <url> [-r <url> ...]` | 将一个或多个 Git 仓库克隆到 `projects/` 并注册 |
| `jumbo workspace import` | 注册 `projects/` 下所有尚未跟踪的目录 |
| `jumbo workspace import -p <name> [-p <name> ...]` | 注册 `projects/` 下指定的目录 |
| `jumbo workspace remove -p <name> [-p <name> ...]` | 删除指定项目目录，并更新工作空间元数据和 IDE 配置 |
| `jumbo workspace remove -p <name> --yes` | 不询问直接移除，即使存在未提交更改 |
| `jumbo workspace sync` | 协调本地项目、元数据、uv 来源和 IDE 配置 |
| `jumbo workspace watch --interval 30` | 持续重复同步，直到用户中断 |
| `jumbo workspace clean` | 清理工作空间以及所有本地已注册项目的生成产物 |

移除项目会删除其目录。未使用 `--yes` 时，如果 Jumbo 检测到未提交更改，会要求确认。

## Python 依赖行为

每个 Python 项目仍需在自己的 `project.dependencies` 中声明依赖。Jumbo 在工作空间根目录维护共享的解析信息：

- 本地 Python 仓库成为 uv workspace 成员，并使用 `{ workspace = true }` 来源；
- 已注册但本地缺失的包可以使用已记录的 Git 远程地址；
- 非 Python 目录会从 uv workspace 中排除；
- Jumbo 只管理 `[tool.jumbo.workspace_sources]` 中列出的来源条目，因此会保留其它根目录配置。

`jumbo.toml` 记录工作空间成员、仓库路径、远程地址、包名与 IDE 设置。应将它视为工作空间元数据，并与工作空间配置一同提交。

## Node.js 项目行为

Node.js / TypeScript 项目通过 `package.json` 识别。当仓库同时包含 `pyproject.toml` 和 `package.json` 时按 Python 项目构建 —— 注册表优先检查 Python，使既有 Python 项目可以为工具链添加 `package.json` 而不改变识别结果。

各步骤依据项目自身配置选择：

- **锁文件与安装：** 每条流程先用 `npm install --package-lock-only --ignore-scripts` 刷新 `package-lock.json`（与指纹引擎 `jumbo lock` 的命令完全一致，因此工作空间构建与锁生成产出同一份文件），再用 `npm ci` 安装。仅解析的步骤绝不执行生命周期脚本；`npm ci` 是真实安装，会正常执行脚本。由于安装总是紧跟一次全新的锁步骤，流程对生成锁的 npm 版本不敏感（npm ≥ 7 生成 lockfileVersion 2/3；旧版 v1 锁会在同一步骤中被重新解析并升级）。
- **构建：** 配置了 `build` 脚本时运行它；没有构建脚本的包执行仅安装的构建。
- **测试：** 配置了真实 `test` 脚本时运行 `npm test`；否则当项目包含 Node 测试文件（`*.test.js` 等）时运行 `node --test`；否则跳过该步骤并给出提示。
- **格式化：** 配置了 `format` / `format:check` 脚本时运行它们；否则在检测到 Prettier 配置（`prettier` 依赖、package.json 中的 `prettier` 键或 Prettier 配置文件）时直接运行 Prettier。`npx --no-install` 只使用本地已安装的二进制，不会下载任何内容。
- **Engines：** 任何步骤运行前都会校验 `engines.node` —— 不满足的范围会立即失败并给出两个版本号；无法静态求值的范围交给 npm 自行检查。
- **Release：** `jumbo release` 是严格变体：构建流程 → 测试 → 严格格式检查。它不会发布；产物发布由发布契约中的执行器负责。

npm 是受支持的包管理器；选择其它工具的 `packageManager` 字段暂不支持。内部依赖使用 `@juntai/*` 作用域（兼容旧版 `@zephytiju/*`），由 `jumbo lock` 与 materializer 以 `file:deps/<slug>` 坐标注入 —— 工作空间流程本身只运行普通工具链。

Node 仓库在 `jumbo.toml` 中以 npm 包身份注册（`package = "@juntai/kit"`、`ecosystem = "node"`），并被排除在 uv workspace 之外，因此 Python + Node 混合工作空间无需改动即可使用：不会生成根级 npm workspace，因为 npm workspace 会把 `node_modules` 集中到根目录，改变 jumbo file 协议注入模型所依赖的单项目安装语义。

## 依赖解析（Jumbo 索引）

内部依赖**只按主版本（major）声明**，并通过 [Jumbo 索引](https://github.com/zephytiju/JumboIndex)（所有已发布内部构建的只追加记录仓库）解析。解析返回声明主版本的最新索引记录（以记录顺序为准，而非时间戳）。第三方依赖原样透传，由常规语言工具链解析。

接受的声明形式：

| 清单文件 | 接受 | 拒绝 |
| --- | --- | --- |
| `pyproject.toml` | `juntai-fuse-api[http]@2`（jumbo 仅主版本形式）、`pkg==2.*`、`pkg~=2.0`、`pkg>=2,<3` | Git URL（`git+https://…`）、直接 wheel/tarball URL、精确锁定（`==2.1.3`）、跨越或未限定单一主版本的范围（`>=2`、`>=1,<3`） |
| `package.json`（`@juntai/*`、旧版 `@zephytiju/*`） | `"1"`、`"1.x"`、`"^1"`、`"~1"`、`">=1,<2"` | Git/tarball URL、`user/repo` 简写、`file:` 路径、精确锁定（`"1.2.3"`）、`">=1"`、`">=1,<3"`、`"*"` |

```bash
# 解析单个声明
jumbo resolve juntai-fuse-api[http]@2
jumbo resolve '@juntai/demo-kit@^1'

# 校验并解析清单中的全部依赖
jumbo resolve --manifest projects/consumer/pyproject.toml
jumbo resolve --manifest projects/console/package.json

# 仅校验声明形式（不查索引记录）
jumbo resolve --manifest pyproject.toml --check
```

索引位置依次取：`--index <路径或URL>`、`JUMBO_INDEX_PATH`（本地克隆，推荐）、`JUMBO_INDEX_URL`、默认 JumboIndex 仓库（通过 `gh` 只读获取；仅接受 `https://github.com` URL）。没有索引记录的内部依赖会以**吸收错误**（absorption error）失败，错误会指明缺失的包与吸收步骤：其仓库必须先纳入 jumbo 流水线才能被消费。

## 锁文件生成与指纹

`jumbo lock` 为清单生成语言锁文件，并将内部依赖从索引注入：每个内部包物化为一个位于**稳定相对路径**（`deps/<包名 slug>/`）的最小源码工程，携带索引记录的名称与版本；清单被改写为指向注入源（Python：`name[extras]==<version>` 加 `[tool.uv.sources]` 路径条目；npm：`"file:deps/<slug>"`），随后由常规语言工具产出锁文件 —— `uv.lock` 用 `uv lock --upgrade`，`package-lock.json` 用 `npm install --package-lock-only --ignore-scripts`。第三方范围每次运行都重新解析。改写记录在 `deps/.jumbo-sources.json`，完全可逆且幂等：连续运行两次产物逐字节一致。

```bash
jumbo lock --manifest projects/consumer/pyproject.toml
jumbo lock --manifest projects/console/package.json
jumbo lock --manifest pyproject.toml --inject-only   # 只注入，不运行 uv/npm
```

`jumbo fingerprint` 计算构建输入指纹 **sha256(自身提交 + 生成锁文件的标准提取)**。绝不哈希锁文件原始字节：标准提取（canonical extract）是构建物化内容的有序、与工具无关视图，因此仅格式变化的锁文件改动（不同的 uv/npm 版本、键序、条目顺序）产生相同提取、不触发重建；任何真实的解析变化都会改变指纹。`--lock` 对既有锁文件做纯本地查询；不带该参数则先生成锁文件再计算指纹。

```bash
# 对既有锁文件的纯本地查询
jumbo fingerprint --lock projects/consumer/uv.lock

# 生成锁文件后计算指纹
jumbo fingerprint --manifest projects/consumer/pyproject.toml
```

报告为 JSON：`commit`、`ecosystem`、`lock`、`canonicalExtract`（即索引记录存储的内容）与 `fingerprint`，并附工作区状态。标准提取格式为 `jumbo-canonical-extract/1`：

```json
{
  "format": "jumbo-canonical-extract/1",
  "entries": [
    { "name": "demo-alpha", "version": "2.4.0", "source": "index", "digest": null, "path": "deps/demo-alpha" },
    { "name": "numpy", "version": "1.26.4", "source": "pypi", "digest": "sha256:…", "path": null }
  ]
}
```

条目按 (name, version, source, digest, path) 排序并去重；`source` 为 `index`（jumbo 注入的内部包，以稳定的 `deps/<slug>` 坐标标识）、`pypi`/`npm`（带完整性摘要的第三方仓库条目）或 `path`（其他本地源）。自身工程被排除 —— 它由自身提交表示。指纹前置输入逐字节为 `<40 位十六进制提交>\n<标准 JSON>`（紧凑、结构体字段序）。

**发布守卫：**只有流水线内干净提交上的构建才允许发布（promote）；脏工作区的本地构建绝不发布。`jumbo fingerprint --promote`（以及后续所有发布模式的操作）在工作区无法归因于 HEAD 提交时直接拒绝。jumbo 生成的产物豁免：`deps/` 下的全部内容、生成的锁文件、以及与 HEAD 的差异仅为 jumbo 记录注入改写的清单。被修改的源码文件或多余未跟踪文件会拒绝发布，并列出违规路径。

## Shell 自动补全

安装脚本会为 zsh、Bash 或 fish 配置动态补全。也可以手动生成静态补全脚本：

```bash
jumbo completions zsh > _jumbo
jumbo completions bash > jumbo.bash
jumbo completions fish > jumbo.fish
```

请将生成的文件安装到对应 shell 要求的位置。运行 `jumbo completions --help` 查看所有受支持的 shell。

## 开发与支持

- [开发指南（英文）](./DEVELOPMENT.md)
- [自动生成的 CLI 参考（英文）](./docs/cli-reference.md)
- 如需完整且与当前版本一致的命令细节，请运行 `jumbo --help` 或 `jumbo <command> --help`。
- [许可证](./LICENSE)
