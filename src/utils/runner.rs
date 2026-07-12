use anyhow::Result;
use colored::Colorize;
use std::process::Command;

/// Run a shell command with a description label. Returns true on success.
pub fn run_step(command: &str, description: &str) -> bool {
    println!("{} {}", "➔".blue().bold(), format!("{}...", description).blue().bold());
    let status = Command::new("sh")
        .arg("-c")
        .arg(command)
        .status();
    match status {
        Ok(s) => s.success(),
        Err(e) => {
            eprintln!("{} Failed to execute command: {}", "✗".red(), e);
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
            "✨ SUCCESS".green().bold(),
            "All steps executed flawlessly.".green()
        );
        std::process::exit(0);
    } else {
        eprintln!(
            "\n{}\n{}\n{}",
            "==== BUILD FAILED ====".red().bold(),
            "💥 CRITICAL ERROR".red().bold(),
            "A step returned a non-zero exit code.".red()
        );
        std::process::exit(1);
    }
}

/// Run a series of commands in sequence. Returns Ok(()) if all succeed.
pub fn run_steps(steps: &[(&str, &str)]) -> Result<()> {
    for (command, description) in steps {
        if !run_step(command, description) {
            anyhow::bail!("Step failed: {}", description);
        }
    }
    Ok(())
}
