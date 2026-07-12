# Jumbo Build

Jumbo Build 是 Juntai 内部统一构建工具，为多仓库工作空间提供一致的构建、测试、格式化与发布体验。基于 Rust + Clap 构建，具备高性能与跨平台能力。

---

## 核心特性

- **统一构建入口** — 同一套命令完成 build / test / format / release
- **多语言插件架构** — 当前支持 Python，通过插件机制可扩展至任意语言
- **工作空间管理** — 统一管理多仓库工作空间，自动同步依赖配置与 IDE 设置
- **跨平台原生二进制** — 编译为 macOS / Linux 原生可执行文件，无需运行时依赖

---

## 前置条件

- macOS 或 Linux
- curl（用于安装脚本）
- Python 项目的额外依赖：uv、ruff、pytest

---

## 安装

使用一键安装脚本，自动检测操作系统并安装到 `~/.local/bin`：

```bash
curl -fsSL https://raw.githubusercontent.com/zephytiju/JumboBuild/main/install.sh | sh
```

安装完成后，确保 `~/.local/bin` 在 PATH 中（安装脚本会自动配置）。

验证安装：

```bash
jumbo --version
```

---

## 使用

### 构建命令

```bash
# 默认构建流水线（lock → sync → build）
jumbo

# 运行测试（构建 + pytest）
jumbo test

# 格式化代码（构建 + ruff format + ruff check --fix）
jumbo format

# 发布检查（构建 + pytest + ruff check 严格模式）
jumbo release
```

### 工作空间管理

```bash
# 创建新工作空间（在当前目录下创建 <name> 文件夹作为工作空间）
jumbo workspace create <name>

# 从已有文件夹创建工作空间（<name> 文件夹必须已存在，仅生成缺失文件）
jumbo workspace create <name> -i

# 克隆仓库到工作空间（支持多个）
jumbo workspace use -r <git-repo-url> [-r <git-repo-url> ...]

# 自动导入 projects/ 下所有项目（推荐）
jumbo workspace import

# 导入指定项目（支持多个，名称位于 projects/ 下）
jumbo workspace import -p <project-name> [-p <project-name> ...]

# 移除项目（同时删除目录、更新元数据、IDE 配置；若有未提交更改会提示确认）
jumbo workspace remove -p <project-name> [-p <project-name> ...]

# 跳过确认直接移除（适合脚本使用）
jumbo workspace remove -p <project-name> --yes

# 同步工作空间配置（更新依赖源等）
jumbo workspace sync -l

# 定时监听并同步（间隔秒数）
jumbo workspace watch -i 30
```

`workspace` 可简写为 `ws`。

### 构建指令表

| 指令 | 步骤 |
|------|------|
| `jumbo`（默认） | lock → sync → build |
| `jumbo test` | lock → sync → build → pytest |
| `jumbo format` | lock → sync → build → ruff format → ruff check --fix |
| `jumbo release` | lock → sync → build → pytest → ruff check（严格模式） |

---

### Shell 自动补全

安装脚本 `install.sh` 会自动配置动态补全（支持 zsh / bash / fish），无需手动设置。

安装后，按 Tab 即可实时补全 `projects/` 下的项目名称（如 `import -p`、`remove -p`）。

如需手动配置：

```bash
# zsh
echo 'source <(COMPLETE=zsh jumbo)' >> ~/.zshrc

# bash
echo 'source <(COMPLETE=bash jumbo)' >> ~/.bashrc

# fish
echo 'COMPLETE=fish jumbo | source' >> ~/.config/fish/completions/jumbo.fish
```

---

## 其它资源

许可证参见 [LICENSE](./LICENSE) 文件。
