# Jumbo Build

[English](./README.md)

Jumbo Build 是 Juntai 统一的项目构建与多仓库工作空间管理命令行工具。它为所有受支持的语言提供一致的构建、测试、格式化、发布检查和清理流程，同时通过轻量的插件接口封装各语言特有的行为。

## 为什么使用 Jumbo Build？

- **统一的项目工作流：** 在所有受支持的语言中使用同一组命令。
- **多仓库工作空间：** 在一个工作空间内克隆、导入、移除、同步和监听多个仓库。
- **本地依赖连接：** 将已检出的 Python 包作为 uv workspace 成员连接；本地缺失时可回退到已记录的 Git 远程地址。
- **原生 CLI：** 为 macOS 和 Linux 提供单个 Rust 二进制文件。
- **可扩展的语言支持：** 实现并注册 `LanguageSupport` 即可接入新的生态系统。

Jumbo Build 目前支持通过 `pyproject.toml` 识别的 Python 项目。

## 前置条件

- x86-64 或 ARM64 架构的 macOS / Linux
- Git
- Rust stable 与 Cargo（安装脚本会从源码构建 Jumbo Build）
- Python 项目需要 [uv](https://docs.astral.sh/uv/)；pytest、Ruff 等项目工具应声明在项目的依赖组中

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

| 命令 | Python 流程 |
| --- | --- |
| `jumbo` 或 `jumbo build` | `uv lock --upgrade` → `uv sync` → `uv build` |
| `jumbo test` | 构建流程 → `uv run pytest -v` |
| `jumbo format` | 构建流程 → `ruff format .` → `ruff check --fix .` |
| `jumbo release` | 构建流程 → `uv run pytest -v` → `ruff check .` |
| `jumbo clean` | 删除当前项目的缓存、覆盖率结果和构建产物 |

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
