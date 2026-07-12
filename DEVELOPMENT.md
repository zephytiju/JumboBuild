# 开发指南

本文档面向 Jumbo Build 内部开发者，涵盖环境搭建、架构设计与语言扩展指南。

---

## 快速开始

```bash
# 克隆仓库
git clone <repo-url> && cd JumboBuild

# 构建开发版本
cargo build

# 构建发布版本
cargo build --release

# 运行并测试 CLI
cargo run -- --help

# 运行单元测试
cargo test
```

## 本地环境搭建

### 前置条件

- Rust stable（推荐通过 rustup 安装）
- macOS 或 Linux

### 验证环境

```bash
cargo run -- --version
cargo run -- workspace --help
```

---

## 架构概览

Jumbo Build 采用模块化设计，核心分为四个层次：

```
src/
├── main.rs              # 入口：CLI 解析与命令分发
├── cli/                  # 命令定义层：clap derive 宏定义所有子命令
│   ├── mod.rs            # Cli struct + Commands enum
│   ├── build.rs          # build / test / format / release 命令执行
│   └── workspace.rs      # workspace (ws) 子命令执行
├── workspace/            # 工作空间核心层
│   ├── mod.rs            # 工作空间操作（create/use/import/sync/watch）
│   ├── metadata.rs       # jumbo.toml 元数据模型与读写
│   ├── detection.rs      # 工作空间根目录检测（向上递归）
│   └── vscode.rs         # VSCode .code-workspace 文件生成
├── language/             # 语言插件层
│   ├── mod.rs            # LanguageSupport trait 定义 + 注册表
│   └── python.rs         # Python 语言支持实现
└── utils/
    ├── mod.rs
    └── runner.rs          # Shell 命令执行器
```

### 核心数据流

1. `main.rs` 解析 CLI 参数，分发到 `cli/` 层
2. `cli/` 层调用 `workspace/` 获取元数据和环境信息
3. `cli/` 层调用 `language/` 插件执行语言特定操作
4. `utils/runner.rs` 负责实际 shell 命令执行与输出

### 技术栈

- **语言**: Rust (edition 2021)
- **CLI 框架**: clap 4 (derive 宏)
- **序列化**: serde + toml (元数据) + serde_json (VSCode workspace)
- **Git 操作**: git2 (原生 libgit2 绑定)
- **错误处理**: anyhow + thiserror
- **终端着色**: colored

---

## 工作空间元数据

工作空间根目录维护 `jumbo.toml` 文件：

```toml
[workspace]
name = "my_workspace"

[[workspace.repositories]]
name = "RepoA"
path = "projects/RepoA"

[[workspace.repositories]]
name = "RepoB"
path = "projects/RepoB"
remote = "https://github.com/org/RepoB.git"

[workspace.ide]
type = "vscode"
```

`sync` 命令会根据 `projects/` 目录的实际状态更新此文件，并在本地模式下将依赖源配置为 workspace 成员。

---

## 语言扩展指南

添加新语言支持只需三步：

### 1. 实现 LanguageSupport trait

在 `src/language/` 下新建文件，例如 `rust_lang.rs`：

```rust
use super::LanguageSupport;
use crate::workspace::metadata::RepoInfo;
use anyhow::Result;
use std::path::Path;

pub struct RustSupport;

impl LanguageSupport for RustSupport {
    fn name(&self) -> &str { "rust" }
    fn detect(&self, repo_path: &Path) -> bool {
        repo_path.join("Cargo.toml").exists()
    }
    fn sync_workspace(&self, ws_root: &Path, repo: &RepoInfo, local: bool) -> Result<()> {
        Ok(()) // 按需实现
    }
    fn build(&self, _repo_path: &Path) -> Result<()> { /* ... */ Ok(()) }
    fn test(&self, _repo_path: &Path) -> Result<()> { /* ... */ Ok(()) }
    fn format(&self, _repo_path: &Path) -> Result<()> { /* ... */ Ok(()) }
    fn release(&self, _repo_path: &Path) -> Result<()> { /* ... */ Ok(()) }
}
```

### 2. 在 language/mod.rs 中声明模块

```rust
pub mod rust_lang;
```

### 3. 注册到 get_registry()

```rust
pub fn get_registry() -> Vec<Box<dyn LanguageSupport>> {
    vec![
        Box::new(python::PythonSupport),
        Box::new(rust_lang::RustSupport),
    ]
}
```

语言检测自动进行——`detect()` 返回 true 的第一个匹配语言将被使用。

---

## 开发规范

### 提交格式

遵循 [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/)：`feat:`, `fix:`, `chore:`, `refactor:` 等。

### 代码风格

- 使用 `cargo fmt` 格式化代码
- 使用 `cargo clippy` 检查代码质量
- 提交前确保 `cargo build` 和 `cargo test` 通过

### 模块组织原则

- **关注点分离**: CLI 定义、工作空间逻辑、语言插件各自独立
- **插件化**: 新语言只需实现 trait + 注册，不修改核心代码
- **自动检测**: 语言识别通过文件签名（如 `pyproject.toml`）自动完成

---

## 核心模块索引

| 领域 | 文件 | 职责 |
|------|------|------|
| CLI 入口 | `cli/mod.rs` | clap 命令结构定义 |
| 构建执行 | `cli/build.rs` | 遍历仓库，调用语言插件执行构建 |
| 工作空间命令 | `cli/workspace.rs` | create/use/import/sync/watch 参数解析 |
| 元数据 | `workspace/metadata.rs` | jumbo.toml 模型定义与 IO |
| 空间检测 | `workspace/detection.rs` | 向上递归查找 workspace root |
| IDE 集成 | `workspace/vscode.rs` | .code-workspace 文件生成 |
| 语言 trait | `language/mod.rs` | LanguageSupport 定义 + 注册表 |
| Python 支持 | `language/python.rs` | Python 构建/测试/格式化/sync |
| 命令执行 | `utils/runner.rs` | shell 命令封装 |
