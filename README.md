# Jumbo Build

Jumbo Build 是 Juntai 内部统一构建系统，旨在为 Juntai 各项目提供一致、可靠的构建与开发体验。当前已支持 Python 项目，未来将扩展至更多编程语言。

![Coverage](./badges/coverage.svg) ![Duration](./badges/duration.svg) ![Last Run](./badges/last-run.svg)
![Skipped](./badges/skipped.svg) ![Tests](./badges/tests.svg) ![Warnings](./badges/warnings.svg) ![XFailed](./badges/xfailed.svg)

---

## 📌 简述

“用 1-2 段文字深入介绍本项目解决的核心问题、目标运行环境以及主要能力。简要提及所采用的关键技术（例如："基于异步消息驱动架构构建，采用 Node.js/TypeScript、RabbitMQ 以及分布式空间索引..."）。”

### 核心特性

- **统一构建入口** — 使用同一套命令完成构建、测试、格式化与发布
- **多语言支持（规划中）** — 当前支持 Python，后续将覆盖更多语言生态
- **开箱即用** — 内置 lockfile 更新、依赖同步、代码质量检查等完整流水线
- **丰富的终端输出** — 基于 Rich 的彩色终端面板，清晰展示构建状态

---

## 🏗️ 架构设计

### 核心架构蓝图
*提供系统数据流转或服务间交互方式的高层概念性概述。*

"图示"

### 组件分解

* **组件 A：** 核心职责。不要罗列完整的模块结构和所有代码元素。多个代码元素可归入一个重要组件。
* **组件 B：** 核心职责。不要罗列完整的模块结构和所有代码元素。多个代码元素可归入一个重要组件。
* **组件 C：** 核心职责。不要罗列完整的模块结构和所有代码元素。多个代码元素可归入一个重要组件。
* ...

### 技术栈与决策

* **运行时/语言：** Python 3.11 保持与其它主要产品同一语言。
* **CLI 框架：** Typer 以及 Rich 提供优雅、好看的终端输出。

## 🚀 快速开始

有关搭建本地开发环境、编写代码以及运行测试套件的详细指南，请参阅 [DEVELOPMENT.md](./DEVELOPMENT.md)。

### 前置条件

* Python >= 3.11
* uv

### 安装

将 `jumbo-build` 添加为项目的开发依赖：

```toml
# pyproject.toml
[dependency-groups]
dev = [
    "jumbo-build @ git+https://github.com/juntai/Jumbo.git",
]
```

然后使用 uv 同步依赖：

```bash
uv sync
```

## 使用

安装完成后，可通过 `uv run jumbo-build` 或 `uv run jumbo` 调用所有可用命令：

```bash
# 运行默认构建流水线（lock → sync → build）
uv run jumbo-build | uv run jumbo

# 运行测试（构建 + pytest）
uv run jumbo-build test | uv run jumbo test

# 格式化代码（构建 + ruff format + ruff check --fix）
uv run jumbo-build format | uv run jumbo format

# 发布检查（构建 + pytest + ruff check）
uv run jumbo-build release | uv run jumbo release
```

### 配置快捷别名 `jumbo`

为了避免每次都输入 `uv run jumbo-build`，可以配置 shell 别名来使用 `jumbo` 作为快捷命令。

在 `~/.zshrc`（或 `~/.bashrc`）中添加：

```bash
alias jumbo='uv run jumbo-build'
```

然后重新加载 shell 配置：

```bash
source ~/.zshrc
```

之后即可使用简洁的命令：

```bash
jumbo            # 默认构建流水线
jumbo test       # 运行测试
jumbo format     # 格式化代码
jumbo release    # 发布检查
```

### 构建指令表

每个指令由一系列步骤（shell 指令）组成，通过 `run_step` 串联执行。任一步骤失败时，后续步骤将跳过，并最终通过 `finalize_build` 输出结果面板。

当前指令及其步骤：

| 指令 | 步骤 |
|------|------|
| `jumbo-build`（默认） | lock → sync → build |
| `jumbo-build test` | lock → sync → build → pytest |
| `jumbo-build format` | lock → sync → build → ruff format → ruff check --fix |
| `jumbo-build release` | lock → sync → build → pytest → ruff check（严格模式） |

## 📖 其它资源

许可证参见 [LICENSE](./LICENSE) 文件。

CI/CD 管道参见 [Pipeline]("pipeline_url")。
