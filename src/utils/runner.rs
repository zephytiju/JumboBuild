use anyhow::Result;
use colored::Colorize;
use std::path::Path;
use std::process::Command;

/// Run a shell command in a given working directory. Returns true on success.
pub fn run_step(command: &str, description: &str, working_dir: &Path) -> bool {
    println!(
        "  {} {}",
        "➔".blue().bold(),
        format!("{}...", description).blue().bold()
    );
    let status = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(working_dir)
        .status();
    match status {
        Ok(s) => s.success(),
        Err(e) => {
            eprintln!("  {} Failed to execute command: {}", "✗".red(), e);
            false
        }
    }
}

/// Print a success or failure banner and exit with the appropriate code.
pub fn finalize_build(success: bool) -> ! {
    if success {
        println!(
            "\n{}\n{}\n{}",
            "==== BUILD SUCCESSFUL ====".green().bold(),
            "SUCCESS".green().bold(),
            "All steps executed flawlessly.".green()
        );
        std::process::exit(0);
    } else {
        eprintln!(
            "\n{}\n{}\n{}",
            "==== BUILD FAILED ====".red().bold(),
            "CRITICAL ERROR".red().bold(),
            "A step returned a non-zero exit code.".red()
        );
        std::process::exit(1);
    }
}

/// Run a series of commands in a given working directory. Returns Ok(()) if all succeed.
pub fn run_steps(steps: &[(&str, &str)], working_dir: &Path) -> Result<()> {
    for (command, description) in steps {
        if !run_step(command, description, working_dir) {
            anyhow::bail!("Step failed: {}", description);
        }
    }
    Ok(())
}
