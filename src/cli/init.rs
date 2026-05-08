//! `coord init` — scaffold a project for use with `coord`.
//!
//! Drops two files into the target directory:
//!
//! * `.mcp.json` — Claude-Code-style MCP server config that points at
//!   `coord mcp`. Cursor, Codex CLI, and other MCP clients accept the
//!   same format (or a near variant) so this works out of the box.
//! * `AGENTS.md` — the protocol every agent in this project should
//!   follow when calling `coord` (heartbeat, scan bulletin, post acks).
//!
//! `--force` overwrites existing files. Without it, present files are
//! left alone and reported.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;

#[derive(Args)]
pub struct InitArgs {
    /// Directory to scaffold (defaults to the current directory).
    #[arg(default_value = ".")]
    pub dir: PathBuf,
    /// Overwrite existing `.mcp.json` / `AGENTS.md` if present.
    #[arg(long)]
    pub force: bool,
    /// Skip writing `.mcp.json` (only drop `AGENTS.md`).
    #[arg(long)]
    pub no_mcp: bool,
    /// Skip writing `AGENTS.md` (only drop `.mcp.json`).
    #[arg(long)]
    pub no_agents: bool,
}

pub fn run(args: &InitArgs) -> Result<()> {
    let root = &args.dir;
    fs::create_dir_all(root).with_context(|| format!("create {root:?}"))?;

    let mut wrote = 0;
    let mut skipped = 0;

    if !args.no_mcp {
        match write_if(root.join(".mcp.json"), MCP_JSON, args.force)? {
            WriteResult::Wrote(p) => {
                println!("  wrote   {}", p.display());
                wrote += 1;
            }
            WriteResult::Skipped(p) => {
                println!(
                    "  skipped {} (already exists; pass --force to overwrite)",
                    p.display()
                );
                skipped += 1;
            }
        }
    }

    if !args.no_agents {
        match write_if(root.join("AGENTS.md"), AGENTS_MD, args.force)? {
            WriteResult::Wrote(p) => {
                println!("  wrote   {}", p.display());
                wrote += 1;
            }
            WriteResult::Skipped(p) => {
                println!(
                    "  skipped {} (already exists; pass --force to overwrite)",
                    p.display()
                );
                skipped += 1;
            }
        }
    }

    println!();
    println!(
        "scaffolded coord into {} ({} written, {} skipped)",
        root.display(),
        wrote,
        skipped
    );
    println!();
    println!("next steps:");
    println!("  1. Start the daemon (in a long-lived terminal):");
    println!("       coord serve --vault .coord/vault");
    println!("  2. In your IDE / agent, point it at this project's .mcp.json");
    println!("     (Claude Code picks it up automatically; see README for");
    println!("     Cursor / Codex / Gemini configs).");
    println!("  3. Watch it live:");
    println!("       coord top");
    Ok(())
}

enum WriteResult {
    Wrote(PathBuf),
    Skipped(PathBuf),
}

fn write_if(path: PathBuf, contents: &str, force: bool) -> Result<WriteResult> {
    if path.exists() && !force {
        return Ok(WriteResult::Skipped(path));
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| format!("create {parent:?}"))?;
        }
    }
    write_atomic(&path, contents)?;
    Ok(WriteResult::Wrote(path))
}

fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents).with_context(|| format!("write {tmp:?}"))?;
    fs::rename(&tmp, path).with_context(|| format!("rename {tmp:?} -> {path:?}"))?;
    Ok(())
}

const MCP_JSON: &str = r#"{
  "mcpServers": {
    "coord": {
      "command": "coord",
      "args": ["mcp"]
    }
  }
}
"#;

const AGENTS_MD: &str = include_str!("../../AGENTS.md");
