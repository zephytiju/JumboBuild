# Jun Build

## 简介

Jun Build 是 Juntai 内部统一构建系统，旨在为 Juntai 各项目提供一致、可靠的构建与开发体验。当前已支持 Python 项目，未来将扩展至更多编程语言。

### 核心特性

- **统一构建入口** — 使用同一套命令完成构建、测试、格式化与发布
- **多语言支持（规划中）** — 当前支持 Python，后续将覆盖更多语言生态
- **开箱即用** — 内置 lockfile 更新、依赖同步、代码质量检查等完整流水线
- **丰富的终端输出** — 基于 Rich 的彩色终端面板，清晰展示构建状态

## 安装

将 `jun-build` 添加为项目的开发依赖：

```toml
# pyproject.toml
[dependency-groups]
dev = [
    "jun-build @ git+https://github.com/juntai/JunBuild.git",
]
```

然后使用 uv 同步依赖：

```bash
uv sync
```

## 使用

安装完成后，可通过 `uv run jun-build` 调用所有可用命令：

```bash
# 运行默认构建流水线（lock → sync → build）
uv run jun-build

# 运行测试（构建 + pytest）
uv run jun-build test

# 格式化代码（构建 + ruff format + ruff check --fix）
uv run jun-build format

# 发布检查（构建 + pytest + ruff check）
uv run jun-build release
```

### 配置快捷别名 `jb`

为了避免每次都输入 `uv run jun-build`，可以配置 shell 别名来使用 `jb` 作为快捷命令。

在 `~/.zshrc`（或 `~/.bashrc`）中添加：

```bash
alias jb='uv run jun-build'
```

然后重新加载 shell 配置：

```bash
source ~/.zshrc
```

之后即可使用简洁的命令：

```bash
jb            # 默认构建流水线
jb test       # 运行测试
jb format     # 格式化代码
jb release    # 发布检查
```

## 开发指南

参见 [DEVELOPMENT](./DEVELOPMENT.md) 文件。

## 许可证

参见 [LICENSE](./LICENSE) 文件。
