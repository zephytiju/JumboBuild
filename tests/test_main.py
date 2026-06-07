from unittest.mock import MagicMock, patch

import pytest
import typer
from typer.testing import CliRunner

from jun_build.main import app, finalize_build, run_step

runner = CliRunner()


# ---------------------------------------------------------------------------
# run_step
# ---------------------------------------------------------------------------


class TestRunStep:
    @patch("jun_build.main.subprocess.run")
    def test_returns_true_on_success(self, mock_run):
        mock_run.return_value = MagicMock(returncode=0)
        assert run_step("echo ok", "Step description") is True
        mock_run.assert_called_once_with("echo ok", shell=True)

    @patch("jun_build.main.subprocess.run")
    def test_returns_false_on_failure(self, mock_run):
        mock_run.return_value = MagicMock(returncode=1)
        assert run_step("false", "Failing step") is False

    @patch("jun_build.main.subprocess.run")
    def test_passes_command_with_shell(self, mock_run):
        mock_run.return_value = MagicMock(returncode=0)
        run_step("uv build", "Building")
        mock_run.assert_called_once_with("uv build", shell=True)


# ---------------------------------------------------------------------------
# finalize_build
# ---------------------------------------------------------------------------


class TestFinalizeBuild:
    def test_success_exits_with_code_0(self):
        with pytest.raises(typer.Exit) as exc_info:
            finalize_build(True)
        assert exc_info.value.exit_code == 0

    def test_failure_exits_with_code_1(self):
        with pytest.raises(typer.Exit) as exc_info:
            finalize_build(False)
        assert exc_info.value.exit_code == 1

    def test_success_prints_success_panel(self, capsys):
        with pytest.raises(typer.Exit):
            finalize_build(True)
        captured = capsys.readouterr()
        assert "SUCCESS" in captured.out

    def test_failure_prints_error_panel(self, capsys):
        with pytest.raises(typer.Exit):
            finalize_build(False)
        captured = capsys.readouterr()
        assert "CRITICAL ERROR" in captured.out


# ---------------------------------------------------------------------------
# CLI commands — verify the correct sequence of steps is invoked
# ---------------------------------------------------------------------------


def _success():
    """Helper: returns a mock subprocess.run result with returncode=0."""
    return MagicMock(returncode=0)


class TestDefaultCommand:
    @patch("jun_build.main.subprocess.run", return_value=_success())
    def test_runs_three_steps(self, mock_run):
        result = runner.invoke(app)
        assert mock_run.call_count == 3
        commands = [call.args[0] for call in mock_run.call_args_list]
        assert commands == [
            "uv lock --upgrade",
            "uv sync",
            "uv build",
        ]

    @patch("jun_build.main.subprocess.run", return_value=_success())
    def test_exits_with_code_0(self, mock_run):
        result = runner.invoke(app)
        assert result.exit_code == 0

    @patch("jun_build.main.subprocess.run")
    def test_stops_on_first_failure(self, mock_run):
        mock_run.return_value = MagicMock(returncode=1)
        result = runner.invoke(app)
        # Only the first step runs because it fails (short-circuit)
        assert mock_run.call_count == 1
        assert result.exit_code == 1


class TestTestCommand:
    @patch("jun_build.main.subprocess.run", return_value=_success())
    def test_runs_four_steps(self, mock_run):
        result = runner.invoke(app, ["test"])
        assert mock_run.call_count == 4
        commands = [call.args[0] for call in mock_run.call_args_list]
        assert commands == [
            "uv lock --upgrade",
            "uv sync",
            "uv build",
            "pytest -v",
        ]

    @patch("jun_build.main.subprocess.run", return_value=_success())
    def test_exits_with_code_0(self, mock_run):
        result = runner.invoke(app, ["test"])
        assert result.exit_code == 0

    @patch("jun_build.main.subprocess.run")
    def test_stops_on_pytest_failure(self, mock_run):
        # First 3 succeed, 4th (pytest) fails
        mock_run.side_effect = [
            MagicMock(returncode=0),
            MagicMock(returncode=0),
            MagicMock(returncode=0),
            MagicMock(returncode=1),
        ]
        result = runner.invoke(app, ["test"])
        assert mock_run.call_count == 4
        assert result.exit_code == 1


class TestFormatCommand:
    @patch("jun_build.main.subprocess.run", return_value=_success())
    def test_runs_five_steps(self, mock_run):
        result = runner.invoke(app, ["format"])
        assert mock_run.call_count == 5
        commands = [call.args[0] for call in mock_run.call_args_list]
        assert commands == [
            "uv lock --upgrade",
            "uv sync",
            "uv build",
            "ruff format .",
            "ruff check --fix .",
        ]

    @patch("jun_build.main.subprocess.run", return_value=_success())
    def test_exits_with_code_0(self, mock_run):
        result = runner.invoke(app, ["format"])
        assert result.exit_code == 0


class TestReleaseCommand:
    @patch("jun_build.main.subprocess.run", return_value=_success())
    def test_runs_five_steps(self, mock_run):
        result = runner.invoke(app, ["release"])
        assert mock_run.call_count == 5
        commands = [call.args[0] for call in mock_run.call_args_list]
        assert commands == [
            "uv lock --upgrade",
            "uv sync",
            "uv build",
            "pytest -v",
            "ruff check .",
        ]

    @patch("jun_build.main.subprocess.run", return_value=_success())
    def test_exits_with_code_0(self, mock_run):
        result = runner.invoke(app, ["release"])
        assert result.exit_code == 0

    @patch("jun_build.main.subprocess.run")
    def test_fails_when_ruff_check_fails(self, mock_run):
        # All succeed except the last ruff check
        mock_run.side_effect = [
            MagicMock(returncode=0),
            MagicMock(returncode=0),
            MagicMock(returncode=0),
            MagicMock(returncode=0),
            MagicMock(returncode=1),
        ]
        result = runner.invoke(app, ["release"])
        assert mock_run.call_count == 5
        assert result.exit_code == 1
