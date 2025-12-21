//! Image build/push helpers for laptop workflows.

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use std::path::PathBuf;

use crate::runner;
#[derive(Subcommand, Debug)]
/// Image workflow commands.
pub enum ImageCommand {
    /// Build and push an image with one or more tags.
    #[command(alias = "push")]
    Publish {
        /// Image prefix without tag (e.g. ghcr.io/me/myapp).
        #[arg(short = 'i', long, help = "Image prefix without tag")]
        image_prefix: String,
        /// Tag(s) to apply; repeatable. If empty, uses git SHA and "latest".
        #[arg(short = 't', long = "tag", help = "Image tag (repeatable)")]
        tags: Vec<String>,
        /// Git ref to resolve when tags are omitted.
        #[arg(short = 'r', long, default_value = "HEAD", help = "Git ref to resolve")]
        git_ref: String,
        /// Dockerfile path.
        #[arg(
            short = 'f',
            long,
            default_value = "Dockerfile",
            help = "Dockerfile path"
        )]
        dockerfile: String,
        /// Build context directory.
        #[arg(
            short = 'C',
            long,
            default_value = ".",
            help = "Build context directory"
        )]
        context: PathBuf,
        /// Skip pushing to the registry.
        #[arg(short = 'P', long, help = "Build only; do not push")]
        no_push: bool,
        /// Print actions without executing.
        #[arg(short = 'D', long, help = "Print actions without executing")]
        dry_run: bool,
    },
}

/// Handle image workflow subcommands.
pub fn handle(command: ImageCommand) -> Result<()> {
    match command {
        ImageCommand::Publish {
            image_prefix,
            tags,
            git_ref,
            dockerfile,
            context,
            no_push,
            dry_run,
        } => publish_image(
            &image_prefix,
            tags,
            &git_ref,
            &dockerfile,
            &context,
            no_push,
            dry_run,
        ),
    }
}

fn publish_image(
    image_prefix: &str,
    mut tags: Vec<String>,
    git_ref: &str,
    dockerfile: &str,
    context: &PathBuf,
    no_push: bool,
    dry_run: bool,
) -> Result<()> {
    if tags.is_empty() {
        let sha = resolve_git_ref(git_ref).unwrap_or_else(|_| "unknown".to_string());
        tags.push(sha);
        tags.push("latest".to_string());
    }
    let primary = tags
        .get(0)
        .context("at least one tag is required")?
        .to_string();
    let primary_ref = format!("{}:{}", image_prefix, primary);
    let mut all_refs = Vec::new();
    for tag in &tags {
        all_refs.push(format!("{}:{}", image_prefix, tag));
    }

    if dry_run {
        println!("dry-run: image publish");
        println!("context={}", context.display());
        println!("dockerfile={}", dockerfile);
        println!("image_prefix={}", image_prefix);
        println!("tags={}", tags.join(","));
        if no_push {
            println!("would skip push");
        } else {
            println!("would push tags: {}", all_refs.join(","));
        }
        return Ok(());
    }

    run_podman(&[
        "build",
        "-t",
        &primary_ref,
        "-f",
        dockerfile,
        context.to_string_lossy().as_ref(),
    ])?;

    for extra in all_refs.iter().skip(1) {
        run_podman(&["tag", &primary_ref, extra])?;
    }

    if !no_push {
        for image in all_refs {
            run_podman(&["push", &image])?;
        }
    }

    println!("published {}", primary_ref);
    Ok(())
}

fn resolve_git_ref(reference: &str) -> Result<String> {
    let repo = git2::Repository::discover(".").context("git repo not found")?;
    let obj = repo
        .revparse_single(reference)
        .context("failed to resolve git ref")?;
    let commit = obj.peel_to_commit().context("failed to peel to commit")?;
    Ok(commit.id().to_string())
}

fn run_podman(args: &[&str]) -> Result<()> {
    let status = runner::run_status("podman", args)
        .with_context(|| format!("failed to run podman {:?}", args))?;
    if status.success() {
        Ok(())
    } else {
        bail!("podman failed: {:?}", args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{Runner, set_runner_for_tests};
    use std::process::{ExitStatus, Output};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingRunner {
        commands: Mutex<Vec<String>>,
    }

    impl Runner for RecordingRunner {
        fn output(&self, program: &str, args: &[&str]) -> anyhow::Result<Output> {
            let args_joined = args.iter().copied().collect::<Vec<&str>>().join(" ");
            let cmdline = format!("{} {}", program, args_joined);
            self.commands.lock().expect("commands lock").push(cmdline);
            Ok(Output {
                status: exit_status(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
    }

    #[cfg(unix)]
    fn exit_status(code: i32) -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(code << 8)
    }

    #[cfg(windows)]
    fn exit_status(code: i32) -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(code as u32)
    }

    #[test]
    fn publish_image_runs_build_tag_push() -> Result<()> {
        let runner = Arc::new(RecordingRunner::default());
        let guard = set_runner_for_tests(runner.clone());

        publish_image(
            "ghcr.io/me/app",
            vec!["v1".to_string(), "latest".to_string()],
            "HEAD",
            "Dockerfile",
            &PathBuf::from("."),
            false,
            false,
        )?;

        let commands = runner.commands.lock().expect("commands lock").clone();
        drop(guard);

        assert!(commands.iter().any(|cmd| cmd.contains("podman build")));
        assert!(commands.iter().any(|cmd| cmd.contains("podman tag")));
        assert!(commands.iter().any(|cmd| cmd.contains("podman push")));
        Ok(())
    }

    #[test]
    fn publish_image_defaults_to_git_sha_and_latest() -> Result<()> {
        let temp = tempfile::TempDir::new()?;
        let repo = git2::Repository::init(temp.path())?;
        let file_path = temp.path().join("README.md");
        std::fs::write(&file_path, "hello")?;
        let mut index = repo.index()?;
        index.add_path(std::path::Path::new("README.md"))?;
        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;
        let sig = git2::Signature::now("Test", "test@example.com")?;
        let commit_id = repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])?;

        let previous_dir = std::env::current_dir()?;
        std::env::set_current_dir(temp.path())?;

        let runner = Arc::new(RecordingRunner::default());
        let guard = set_runner_for_tests(runner.clone());
        publish_image(
            "ghcr.io/me/app",
            Vec::new(),
            "HEAD",
            "Dockerfile",
            &PathBuf::from("."),
            false,
            false,
        )?;
        let commands = runner.commands.lock().expect("commands lock").clone();
        drop(guard);

        std::env::set_current_dir(previous_dir)?;

        let sha = commit_id.to_string();
        assert!(
            commands
                .iter()
                .any(|cmd| cmd.contains(&format!("ghcr.io/me/app:{}", sha)))
        );
        assert!(
            commands
                .iter()
                .any(|cmd| cmd.contains("ghcr.io/me/app:latest"))
        );
        Ok(())
    }
}
