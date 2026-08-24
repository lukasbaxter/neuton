//! Locating the vanilla jar, its libraries, and a JDK, then invoking the
//! vanilla data generator that ships inside the jar.
//!
//! 26.x jars are unobfuscated, so `net.minecraft.data.Main` is callable
//! directly and emits authoritative reports. No mappings, no reverse
//! engineering, no third-party protocol tables to drift out of date.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The version we target. Single-version by design for now.
pub const VERSION: &str = "26.2";

pub struct Env {
    pub mc_dir: PathBuf,
    pub java: PathBuf,
    pub repo: PathBuf,
    pub version: String,
    pub protocol: i64,
}

type Err = Box<dyn std::error::Error>;

impl Env {
    pub fn resolve(mc_dir: Option<PathBuf>, java: Option<PathBuf>) -> Result<Self, Err> {
        let mc_dir = match mc_dir {
            Some(d) => d,
            None => default_mc_dir().ok_or("could not find a .minecraft directory; pass --mc-dir")?,
        };
        let jar = mc_dir.join("versions").join(VERSION).join(format!("{VERSION}.jar"));
        if !jar.exists() {
            return Err(format!(
                "vanilla {VERSION} jar not found at {}\nrun the official launcher once to download it",
                jar.display()
            )
            .into());
        }

        let java = match java {
            Some(j) => j,
            None => find_java().ok_or("no Java 25+ runtime found; pass --java <path>")?,
        };

        let protocol = read_protocol(&jar)?;
        let repo = repo_root()?;

        Ok(Self { mc_dir, java, repo, version: VERSION.to_string(), protocol })
    }

    pub fn jar(&self) -> PathBuf {
        self.mc_dir.join("versions").join(&self.version).join(format!("{}.jar", self.version))
    }

    /// Builds a classpath of every library the launcher downloaded, plus the
    /// client jar itself.
    pub fn classpath(&self) -> Result<String, Err> {
        let mut jars = Vec::new();
        collect_jars(&self.mc_dir.join("libraries"), &mut jars)?;
        if jars.is_empty() {
            return Err("no libraries found; run the official launcher once".into());
        }
        jars.sort();
        jars.push(self.jar());
        let sep = if cfg!(windows) { ';' } else { ':' };
        Ok(jars.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(&sep.to_string()))
    }

    /// Runs the vanilla `--reports` generator and returns the reports directory.
    ///
    /// Cached: skipped entirely if the reports are newer than the jar.
    pub fn run_vanilla_datagen(&self) -> Result<PathBuf, Err> {
        let out = self.repo.join("target").join("datagen").join(&self.version);
        let reports = out.join("reports");
        if is_fresh(&reports, &self.jar()) {
            println!("  vanilla datagen: cached");
            return Ok(reports);
        }

        println!("  vanilla datagen: running net.minecraft.data.Main --reports");
        let status = Command::new(&self.java)
            .arg("-XX:+UseSerialGC") // short-lived one-shot; serial GC starts fastest
            .arg("-cp")
            .arg(self.classpath()?)
            .arg("net.minecraft.data.Main")
            .arg("--reports")
            .arg("--output")
            .arg(&out)
            .stdout(std::process::Stdio::null())
            .status()?;
        if !status.success() {
            return Err(format!("vanilla datagen failed with {status}").into());
        }
        if !reports.exists() {
            return Err(format!("datagen produced no reports at {}", reports.display()).into());
        }
        Ok(reports)
    }
}

/// True if `dir` exists and is newer than `src`.
fn is_fresh(dir: &Path, src: &Path) -> bool {
    let newer = |a: &Path, b: &Path| -> Option<bool> {
        Some(a.metadata().ok()?.modified().ok()? >= b.metadata().ok()?.modified().ok()?)
    };
    dir.join("packets.json").exists() && newer(&dir.join("packets.json"), src).unwrap_or(false)
}

fn collect_jars(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_jars(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "jar") {
            out.push(path);
        }
    }
    Ok(())
}

/// Reads `protocol_version` out of `version.json` inside the jar.
///
/// Done by hand rather than with a zip crate: it is one stored entry and this
/// keeps the datagen tool dependency-light.
fn read_protocol(jar: &Path) -> Result<i64, Err> {
    let out = Command::new("unzip").arg("-p").arg(jar).arg("version.json").output()?;
    if !out.status.success() {
        return Err("could not read version.json from the vanilla jar".into());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    v.get("protocol_version")
        .and_then(|p| p.as_i64())
        .ok_or_else(|| "version.json has no protocol_version".into())
}

fn default_mc_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let home = PathBuf::from(home);
    let candidates = if cfg!(target_os = "macos") {
        vec![home.join("Library/Application Support/minecraft")]
    } else if cfg!(windows) {
        vec![PathBuf::from(std::env::var_os("APPDATA")?).join(".minecraft")]
    } else {
        vec![home.join(".minecraft")]
    };
    candidates.into_iter().find(|p| p.is_dir())
}

/// Prefers an explicit JDK 25/26 over whatever `java` happens to be on PATH,
/// since 26.2 class files are version 69.
fn find_java() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![
        "/opt/homebrew/opt/openjdk@25/bin/java".into(),
        "/opt/homebrew/opt/openjdk@26/bin/java".into(),
        "/opt/homebrew/opt/openjdk/bin/java".into(),
        "/usr/lib/jvm/java-25-openjdk/bin/java".into(),
    ];
    if let Some(paths) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&paths).map(|p| p.join("java")));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Walks up from the compiled binary's manifest dir to the workspace root.
fn repo_root() -> Result<PathBuf, Err> {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if dir.join("Cargo.toml").exists() && dir.join("crates").is_dir() {
            return Ok(dir);
        }
        if !dir.pop() {
            return Err("could not locate workspace root".into());
        }
    }
}
