# Jun Build 开发指南

## 环境设置

### 前置要求

- Python >= 3.11
- [uv](https://docs.astral.sh/uv/) — 用于依赖管理与构建

### 依赖安装

克隆仓库后，使用 uv 安装所有开发依赖：

```bash
uv sync
```

### 本地验证

安装完成后，可直接运行以验证环境是否正常：

```bash
uv run jun-build --help
```

---

## 项目架构

```
src/jun_build/
├── __init__.py
└── main.py        # CLI 入口，所有命令均在此定义
```

### 技术栈

| 依赖 | 用途 |
|------|------|
| [Typer](https://typer.tiangolo.com/) | CLI 框架，基于类型注解自动解析命令与参数 |
| [Rich](https://rich.readthedocs.io/) | 终端美化输出（进度面板、彩色文本） |
| [pytest](https://pytest.org/) | 单元测试框架 |
| [Ruff](https://docs.astral.sh/ruff/) | 代码格式化与 Lint |

### 命令流水线设计

每个命令由一系列步骤（shell 命令）组成，通过 `run_step` 串联执行。任一步骤失败时，后续步骤将跳过，并最终通过 `finalize_build` 输出结果面板。

当前命令及其步骤：

| 命令 | 步骤 |
|------|------|
| `jun-build`（默认） | lock → sync → build |
| `jun-build test` | lock → sync → build → pytest |
| `jun-build format` | lock → sync → build → ruff format → ruff check --fix |
| `jun-build release` | lock → sync → build → pytest → ruff check（严格模式） |

---

## 添加新命令

在 `main.py` 中使用 `@app.command()` 装饰器注册新命令：

```python
@app.command()
def my_new_command():
    """描述此命令的用途（会显示在 --help 中）。"""
    success = (
        run_step("uv lock --upgrade", "Updating lockfile")
        and run_step("uv sync", "Syncing environment metadata")
        and run_step("uv build", "Running python build")
        and run_step("your-command-here", "描述此步骤")
    )
    finalize_build(success)
```

**约定：**
- 每个命令的前三步（lock → sync → build）保持一致，确保环境最新
- 使用短路求值（`and`）实现步骤失败时自动终止
- 最后必须调用 `finalize_build(success)` 输出结果

---

## 测试

### 运行测试

```bash
# 直接运行 pytest
uv run pytest -v

# 或通过 jun-build 命令（会先执行构建流水线）
uv run jun-build test
```

### 编写测试

测试文件放在 `tests/` 目录下，文件名以 `test_` 开头：

```python
# tests/test_my_feature.py
def test_example():
    assert True
```

**约定：**
- 测试函数命名：`test_<功能描述>`
- 每个测试函数只验证一个行为
- 需要时可在 `tests/conftest.py` 中定义共享 fixture

---

## 代码规范

### 格式化与 Lint

使用 Ruff 统一代码风格：

```bash
# 格式化代码
uv run ruff format .

# 检查并自动修复
uv run ruff check --fix .
```

或通过 jun-build 一键执行：

```bash
uv run jun-build format
```

### Ruff 配置

配置位于 `pyproject.toml`：

- `target-version = "py310"` — 兼容 Python 3.10+ 语法
- `extend-select = ["C4"]` — 在默认规则基础上额外启用 `flake8-comprehensions`

---

## 发布流程

发布前需通过完整检查：

```bash
# 执行测试 + 严格 lint（无自动修复）
uv run jun-build release
```

`release` 命令会依次执行：lock → sync → build → pytest → ruff check（严格模式，不会自动修复），全部通过后方可发布。

### 版本号管理

版本号维护在 `pyproject.toml` 的 `version` 字段，遵循 [SemVer](https://semver.org/) 规范。
