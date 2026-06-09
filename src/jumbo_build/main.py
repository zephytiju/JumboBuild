import subprocess
import typer
from rich.console import Console
from rich.panel import Panel

app = typer.Typer(
    help="🚀 Jun Build is the Juntai internal unified build tool.",
    rich_markup_mode="rich",
)
console = Console()


def run_step(command: str, description: str) -> bool:
    """Helper to run shell steps and print progress."""
    console.print(f"[bold blue]➔ {description}...[/bold blue]")
    result = subprocess.run(command, shell=True)
    return result.returncode == 0


def finalize_build(success: bool):
    """Prints a beautiful success or failure panel matching the final status."""
    if success:
        console.print(
            Panel(
                "[bold green]✨ SUCCESS[/bold green] - All steps executed flawlessly.",
                title="[bold green]==== BUILD SUCCESSFUL ====[/bold green]",
                expand=False,
                border_style="green",
            )
        )
        raise typer.Exit(code=0)
    else:
        console.print(
            Panel(
                "[bold red]💥 CRITICAL ERROR[/bold red] - A step returned a non-zero exit code.",
                title="[bold red]==== BUILD FAILED ====[/bold red]",
                expand=False,
                border_style="red",
            )
        )
        raise typer.Exit(code=1)


@app.callback(invoke_without_command=True)
def main(ctx: typer.Context):
    """
    Main entry point. Runs the default build pipeline if no subcommand is passed.
    """
    if ctx.invoked_subcommand is not None:
        return
    success = (
        run_step("uv lock --upgrade", "Updating lockfile")
        and run_step("uv sync", "Syncing environment metadata")
        and run_step("uv build", "Running python build")
    )
    finalize_build(success)


@app.command()
def test():
    """Run environment updates followed by pytest unit tests."""
    success = (
        run_step("uv lock --upgrade", "Updating lockfile")
        and run_step("uv sync", "Syncing environment metadata")
        and run_step("uv build", "Running python build")
        and run_step("pytest -v", "Executing rigorous testing suite")
    )
    finalize_build(success)


@app.command()
def format():
    """Format local files and resolve code style corrections via Ruff."""
    success = (
        run_step("uv lock --upgrade", "Updating lockfile")
        and run_step("uv sync", "Syncing environment metadata")
        and run_step("uv build", "Running python build")
        and run_step("ruff format .", "Structuring formats")
        and run_step("ruff check --fix .", "Applying automated code lint fixes")
    )
    finalize_build(success)


@app.command()
def release():
    """Execute complete testing validation and run strict linter checks."""
    success = (
        run_step("uv lock --upgrade", "Updating lockfile")
        and run_step("uv sync", "Syncing environment metadata")
        and run_step("uv build", "Running python build")
        and run_step("pytest -v", "Executing rigorous testing suite")
        and run_step("ruff check .", "Validating strict rule compliance checks")
    )
    finalize_build(success)


if __name__ == "__main__":
    app()
