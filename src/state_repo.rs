//! Snapshots the file-backed `Store`'s data directory into its own git
//! repo, separate from `mazz-flux-bot`'s own checkout, so a flux/vape
//! instance's accumulated project state survives the pod being torn down.
//!
//! The data directory (see `store.rs`) lives as a sibling of this repo by
//! default, e.g. `../mazz-flux-bot-state`. This module shells out to `git`
//! inside that directory — `git init` on first use, `git add -A && git
//! commit` on demand, and a `git push` if a remote is configured.

use anyhow::{Context, Result};
use std::path::Path;
use tokio::process::Command;

/// Runs `git <args>` with `cwd` as the working directory, returning stdout
/// on success or an error containing stderr on failure.
async fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .with_context(|| format!("failed to run git {args:?} in {}", cwd.display()))?;
    if !out.status.success() {
        anyhow::bail!("git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// True if `dir/.git` exists.
async fn is_git_repo(dir: &Path) -> bool {
    tokio::fs::metadata(dir.join(".git")).await.is_ok()
}

/// Initializes `dir` as its own git repo if it isn't one already: `git
/// init`, sets `user.name`/`user.email` from `GIT_USER_NAME`/`GIT_USER_EMAIL`
/// if those env vars are set and no repo-local identity exists yet, and adds
/// an `origin` remote from `MAZZ_FLUX_STATE_REPO_URL` if that env var is set
/// and no remote named `origin` exists yet.
///
/// Safe to call on every startup — each step is a no-op if already done.
pub async fn ensure_init(dir: &Path) -> Result<()> {
    tokio::fs::create_dir_all(dir).await.with_context(|| format!("creating {}", dir.display()))?;

    if !is_git_repo(dir).await {
        git(dir, &["init"]).await?;
        tracing::info!(dir = %dir.display(), "initialized state repo");
    }

    if let Ok(name) = std::env::var("GIT_USER_NAME") {
        if git(dir, &["config", "user.name"]).await.is_err() {
            git(dir, &["config", "user.name", &name]).await?;
        }
    }
    if let Ok(email) = std::env::var("GIT_USER_EMAIL") {
        if git(dir, &["config", "user.email"]).await.is_err() {
            git(dir, &["config", "user.email", &email]).await?;
        }
    }

    if let Ok(url) = std::env::var("MAZZ_FLUX_STATE_REPO_URL") {
        let has_remote = git(dir, &["remote"]).await.map(|out| out.lines().any(|l| l == "origin")).unwrap_or(false);
        if !has_remote {
            git(dir, &["remote", "add", "origin", &url]).await?;
            tracing::info!(url, "added origin remote to state repo");
        }
    }

    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CommitSummary {
    pub committed: bool,
    pub message: String,
    pub sha: Option<String>,
    pub pushed: bool,
}

/// Stages every change in the state repo and commits it. Returns
/// `committed: false` if there was nothing to commit. Pushes to `origin
/// <branch>` (default branch `main`, override `MAZZ_FLUX_STATE_BRANCH`) only
/// if an `origin` remote is configured — local-only commits still work
/// with no remote set up yet.
pub async fn commit(dir: &Path, message: &str) -> Result<CommitSummary> {
    ensure_init(dir).await?;

    git(dir, &["add", "-A"]).await?;

    // `git status --porcelain` is empty when there's nothing staged.
    let status = git(dir, &["status", "--porcelain"]).await?;
    if status.is_empty() {
        return Ok(CommitSummary { committed: false, message: "nothing to commit".to_string(), sha: None, pushed: false });
    }

    git(dir, &["commit", "-m", message]).await?;
    let sha = git(dir, &["rev-parse", "HEAD"]).await.ok();

    let has_remote = git(dir, &["remote"]).await.map(|out| out.lines().any(|l| l == "origin")).unwrap_or(false);
    let mut pushed = false;
    if has_remote {
        let branch = std::env::var("MAZZ_FLUX_STATE_BRANCH").unwrap_or_else(|_| "main".to_string());
        match git(dir, &["push", "origin", &format!("HEAD:{branch}")]).await {
            Ok(_) => pushed = true,
            Err(e) => tracing::warn!(error = %e, "state repo commit succeeded but push failed"),
        }
    }

    Ok(CommitSummary { committed: true, message: message.to_string(), sha, pushed })
}
