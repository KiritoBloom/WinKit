//! Bounded workspace scanning for `workspace_snapshot` and `diagnose_workspace`
//!.
//!
//! This module is pure `std` — it never touches Win32 and never executes shell
//! commands. It walks a workspace directory with hard depth/file budgets,
//! detects repository metadata, manifests, package managers, languages, and
//! frameworks from *file names and bounded manifest reads*, and deliberately
//! does not read source files or secret files.
//!
//! Security rules enforced here:
//!   * The scan root is canonicalized by the caller and asserted to be a
//!     directory; whole-drive scans are rejected by the caller's allow/deny
//!     policy and by the depth/file budgets.
//!   * Secret-bearing files (.env, key material, credential stores, npmrc)
//!     are never opened; they are *listed* as excluded so the agent can see
//!     the data category boundary.
//!   * Manifest reads are capped in bytes; `package.json` scripts are
//!     returned by name only (no bodies, no environment expansion).
//!   * Git metadata is read from the repository's own files (`.git/HEAD`,
//!     `.git/config`) with bounded reads; no `git` process is spawned and
//!     remote URLs are redacted.

use crate::utils::redact::{redact_url_userinfo, redact_value};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Maximum bytes read from one manifest file.
const MAX_MANIFEST_BYTES: usize = 32 * 1024;
/// Maximum bytes read from one `.git/*` metadata file.
const MAX_GIT_BYTES: usize = 16 * 1024;
/// Maximum number of excluded secret-file names reported.
const MAX_EXCLUDED_REPORTED: usize = 20;
/// Maximum number of scripts reported from a root manifest.
const MAX_SCRIPTS: usize = 40;

/// What kind of project manifest a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestKind {
    Cargo,
    Npm,
    Pnpm,
    Yarn,
    Bun,
    Pip,
    Pyproject,
    Poetry,
    Uv,
    GoMod,
    Maven,
    Gradle,
    Nuget,
    Csproj,
    Sln,
    Cmake,
    Dockerfile,
    Other,
}

impl ManifestKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::Pnpm => "pnpm",
            Self::Yarn => "yarn",
            Self::Bun => "bun",
            Self::Pip => "pip",
            Self::Pyproject => "pyproject",
            Self::Poetry => "poetry",
            Self::Uv => "uv",
            Self::GoMod => "go_modules",
            Self::Maven => "maven",
            Self::Gradle => "gradle",
            Self::Nuget => "nuget",
            Self::Csproj => "csproj",
            Self::Sln => "sln",
            Self::Cmake => "cmake",
            Self::Dockerfile => "dockerfile",
            Self::Other => "other",
        }
    }
}

/// One detected manifest, positioned relative to the scan root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestEntry {
    /// Path relative to the scan root.
    pub path: String,
    pub kind: ManifestKind,
    /// Package/project name when the manifest declares one (bounded).
    pub name: Option<String>,
}

/// Safe repository metadata read from the repository's own files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepositoryMetadata {
    pub has_git: bool,
    /// Current branch (from `.git/HEAD`), or `None` for detached HEAD.
    pub branch: Option<String>,
    /// Detached commit / branch-symbolic pointer when derivable.
    pub head_ref: Option<String>,
    /// Redacted `origin` remote URL (userinfo stripped, secrets masked).
    pub remote_origin: Option<String>,
    /// Why no dirty-state report exists: dirty state is never computed
    /// because WinKit does not execute `git`.
    pub dirty_state: String,
}

/// The bounded result of one workspace scan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceScan {
    /// Canonical scan root (as provided; the caller canonicalizes).
    pub root: String,
    /// Last path component, for display.
    pub display_name: String,
    /// Nearest ancestor containing a `.git` directory, when found.
    pub repo_root: Option<String>,
    pub repository: RepositoryMetadata,
    pub package_managers: Vec<String>,
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
    pub manifests: Vec<ManifestEntry>,
    /// Lockfile names (relative paths), bounded.
    pub lockfiles: Vec<String>,
    /// `package.json` script *names* from the root manifest, bounded.
    pub scripts: Vec<String>,
    /// Directories recognized as build/cache output (not descended into).
    pub build_dirs: Vec<String>,
    /// `Dockerfile`/`compose.yaml` style files.
    pub docker_files: Vec<String>,
    /// Number of directory entries examined.
    pub entries_scanned: usize,
    /// True when the depth or file budget stopped the walk early.
    pub truncated: bool,
    /// Secret-bearing files detected but never opened (bounded list).
    pub excluded_secret_files: Vec<String>,
    /// Whether the scan root itself exists and is a directory.
    pub root_is_valid: bool,
    /// Scan wall time (ms).
    pub scan_ms: u64,
}

/// Directories that are never descended into (build output, vendored deps,
/// hidden VCS internals).
pub const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    ".hg",
    ".svn",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".output",
    ".cache",
    ".turbo",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".gradle",
    ".idea",
    ".vscode",
    ".vs",
    ".terraform",
    ".aws",
    ".ssh",
    ".kube",
    ".azure",
];

/// File-name patterns that mark a file as secret-bearing. Matched
/// case-insensitively against the file name; files matching are never opened.
pub fn is_secret_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower == ".env" || lower.starts_with(".env.") {
        return true;
    }
    if lower == ".npmrc" || lower == ".yarnrc.yml" || lower == ".git-credentials" {
        return true;
    }
    if lower == "credentials" || lower == "credentials.json" {
        return true;
    }
    if lower == "id_rsa"
        || lower == "id_dsa"
        || lower == "id_ed25519"
        || lower == "id_ecdsa"
        || lower == "netrc"
        || lower == "_netrc"
    {
        return true;
    }
    if lower == "secrets.yaml" || lower == "secrets.yml" || lower == "secret.yaml" {
        return true;
    }
    lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
        || lower.ends_with(".pfx")
        || lower.ends_with(".ppk")
        || lower.ends_with(".jks")
        || lower.ends_with(".keystore")
        || lower.ends_with("service-account.json")
        || lower.ends_with("service_account.json")
}

/// Classify a file name into a manifest kind, when WinKit recognizes it.
pub fn manifest_kind_for(file_name: &str) -> Option<ManifestKind> {
    let lower = file_name.to_ascii_lowercase();
    match lower.as_str() {
        "cargo.toml" => Some(ManifestKind::Cargo),
        "package.json" => Some(ManifestKind::Npm),
        "pnpm-lock.yaml" => Some(ManifestKind::Pnpm),
        "yarn.lock" => Some(ManifestKind::Yarn),
        "bun.lockb" => Some(ManifestKind::Bun),
        "requirements.txt" => Some(ManifestKind::Pip),
        "pyproject.toml" => Some(ManifestKind::Pyproject),
        "poetry.lock" => Some(ManifestKind::Poetry),
        "uv.lock" => Some(ManifestKind::Uv),
        "go.mod" => Some(ManifestKind::GoMod),
        "pom.xml" => Some(ManifestKind::Maven),
        "build.gradle" | "settings.gradle" | "build.gradle.kts" | "settings.gradle.kts" => {
            Some(ManifestKind::Gradle)
        }
        "packages.config" => Some(ManifestKind::Nuget),
        "cmakelists.txt" => Some(ManifestKind::Cmake),
        "dockerfile" | "dockerfile.dev" | "dockerfile.prod" => Some(ManifestKind::Dockerfile),
        "compose.yaml" | "compose.yml" | "docker-compose.yaml" | "docker-compose.yml" => {
            Some(ManifestKind::Dockerfile)
        }
        _ => {
            if lower.ends_with(".csproj") {
                Some(ManifestKind::Csproj)
            } else if lower.ends_with(".sln") {
                Some(ManifestKind::Sln)
            } else if lower.ends_with(".vcxproj") {
                Some(ManifestKind::Csproj)
            } else if lower.ends_with("package-lock.json") {
                Some(ManifestKind::Npm)
            } else {
                None
            }
        }
    }
}

/// Is this a lockfile name (reported separately from manifests)?
pub fn is_lockfile_name(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "cargo.lock"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "bun.lockb"
            | "poetry.lock"
            | "uv.lock"
            | "go.sum"
            | "composer.lock"
            | "gemfile.lock"
            | "packages.lock.json"
    )
}

/// Scan options (bounded by configuration).
#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub max_depth: u32,
    pub max_files: usize,
    pub include_git: bool,
    pub include_manifests: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_depth: 8,
            max_files: 2000,
            include_git: true,
            include_manifests: true,
        }
    }
}

/// Scan `root` with bounded depth and file counts. `root` must already be
/// canonicalized by the caller. Returns a bounded [`WorkspaceScan`]; a
/// missing/empty root yields `root_is_valid: false` instead of an error so
/// the caller can produce a `limited` report.
pub fn scan_workspace(root: &Path, options: &ScanOptions) -> WorkspaceScan {
    let started = std::time::Instant::now();
    // Windows canonicalization yields the extended-length `\\?\` prefix; strip it
    // so reported paths match what the user sees in Explorer and terminals.
    let clean_root = PathBuf::from(root.to_string_lossy().trim_start_matches("\\\\?\\"));
    let root = clean_root.as_path();
    let mut scan = WorkspaceScan {
        root: root.to_string_lossy().into_owned(),
        display_name: root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.to_string_lossy().into_owned()),
        repo_root: None,
        repository: RepositoryMetadata {
            has_git: false,
            branch: None,
            head_ref: None,
            remote_origin: None,
            dirty_state: "not_computed_without_executing_git".to_string(),
        },
        package_managers: Vec::new(),
        languages: Vec::new(),
        frameworks: Vec::new(),
        manifests: Vec::new(),
        lockfiles: Vec::new(),
        scripts: Vec::new(),
        build_dirs: Vec::new(),
        docker_files: Vec::new(),
        entries_scanned: 0,
        truncated: false,
        excluded_secret_files: Vec::new(),
        root_is_valid: root.is_dir(),
        scan_ms: 0,
    };
    if !scan.root_is_valid {
        scan.scan_ms = started.elapsed().as_millis() as u64;
        return scan;
    }

    let mut budget = options.max_files.max(1);
    let mut pending: Vec<(PathBuf, u32)> = vec![(root.to_path_buf(), 0)];
    // Language weights collected by file extension during the walk.
    let mut language_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut framework_markers: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();

    while let Some((dir, depth)) = pending.pop() {
        if depth > options.max_depth {
            scan.truncated = true;
            continue;
        }
        if budget == 0 {
            scan.truncated = true;
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if budget == 0 {
                scan.truncated = true;
                break;
            }
            budget -= 1;
            scan.entries_scanned += 1;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };

            if file_type.is_dir() {
                if SKIP_DIRS.iter().any(|d| name.eq_ignore_ascii_case(d)) {
                    scan.build_dirs.push(rel(root, &path));
                    continue;
                }
                pending.push((path, depth + 1));
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            language_set_for(&name, &mut language_set);

            if is_secret_file(&name) {
                if scan.excluded_secret_files.len() < MAX_EXCLUDED_REPORTED {
                    scan.excluded_secret_files.push(rel(root, &path));
                }
                continue;
            }

            if options.include_manifests {
                if let Some(kind) = manifest_kind_for(&name) {
                    let name_opt = read_manifest_name(&path, kind);
                    if matches!(
                        kind,
                        ManifestKind::Cargo
                            | ManifestKind::Npm
                            | ManifestKind::Pnpm
                            | ManifestKind::Yarn
                    ) && name_opt.is_some()
                        && scan.manifests.iter().any(|m| m.kind == kind)
                        && is_lockfile_name(&name)
                    {
                        // package-lock.json is classified as npm manifest but
                        // carries no name; report it as a lockfile instead.
                        scan.lockfiles.push(rel(root, &path));
                        continue;
                    }
                    scan.manifests.push(ManifestEntry {
                        path: rel(root, &path),
                        kind,
                        name: name_opt,
                    });
                    match kind {
                        ManifestKind::Cargo => {
                            scan.package_managers.push("cargo".to_string());
                            scan.languages.push("rust".to_string());
                        }
                        ManifestKind::Npm
                        | ManifestKind::Pnpm
                        | ManifestKind::Yarn
                        | ManifestKind::Bun => {
                            scan.package_managers.push(kind.as_str().to_string());
                            scan.languages.push("javascript".to_string());
                            scan.languages.push("typescript".to_string());
                            if kind == ManifestKind::Npm {
                                if let Some(deps) = read_npm_deps(&path) {
                                    deps.iter()
                                        .map(|d| d.to_string())
                                        .collect::<Vec<_>>()
                                        .into_iter()
                                        .for_each(|d| {
                                            dependency_frameworks(&d).into_iter().for_each(|f| {
                                                framework_markers.insert(f.to_string());
                                            });
                                        });
                                }
                            }
                        }
                        ManifestKind::Pip
                        | ManifestKind::Pyproject
                        | ManifestKind::Poetry
                        | ManifestKind::Uv => {
                            if !scan.package_managers.iter().any(|p| p == "pip") {
                                scan.package_managers.push("pip".to_string());
                            }
                            scan.languages.push("python".to_string());
                        }
                        ManifestKind::GoMod => {
                            scan.package_managers.push("go modules".to_string());
                            scan.languages.push("go".to_string());
                        }
                        ManifestKind::Maven => {
                            scan.package_managers.push("maven".to_string());
                            scan.languages.push("java".to_string());
                        }
                        ManifestKind::Gradle => {
                            scan.package_managers.push("gradle".to_string());
                            scan.languages.push("java".to_string());
                        }
                        ManifestKind::Nuget => {
                            scan.package_managers.push("nuget".to_string());
                            scan.languages.push("csharp".to_string());
                        }
                        ManifestKind::Csproj => {
                            scan.languages.push("csharp".to_string());
                        }
                        ManifestKind::Sln => {
                            scan.languages.push("csharp".to_string());
                        }
                        ManifestKind::Cmake => {
                            scan.package_managers.push("cmake".to_string());
                            scan.languages.push("cpp".to_string());
                        }
                        ManifestKind::Dockerfile => {
                            scan.docker_files.push(rel(root, &path));
                        }
                        ManifestKind::Other => {}
                    }
                    continue;
                }
                if is_lockfile_name(&name) {
                    scan.lockfiles.push(rel(root, &path));
                    continue;
                }
            }

            framework_markers_for_file(&name, &mut framework_markers);
        }
    }

    // Root-level package.json scripts (names only, bounded).
    if options.include_manifests {
        let root_package_json = root.join("package.json");
        if root_package_json.is_file() {
            scan.scripts = read_scripts(&root_package_json);
        }
    }

    if options.include_git {
        scan.repo_root = detect_repo_root(root);
        if let Some(repo) = &scan.repo_root {
            scan.repository = read_git_metadata(Path::new(repo));
        }
    }

    scan.package_managers.sort();
    scan.package_managers.dedup();
    scan.languages.sort();
    scan.languages.dedup();
    scan.frameworks = framework_markers.into_iter().collect();
    scan.frameworks.sort();
    scan.manifests.sort_by(|a, b| a.path.cmp(&b.path));
    scan.lockfiles.sort();
    scan.lockfiles.dedup();
    scan.scan_ms = started.elapsed().as_millis() as u64;
    scan
}

/// Relative path with forward slashes for stable serialization.
fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Map a file name to a coarse language signal.
fn language_set_for(name: &str, set: &mut std::collections::BTreeSet<String>) {
    let lower = name.to_ascii_lowercase();
    let lang = if lower.ends_with(".rs") {
        Some("rust")
    } else if lower.ends_with(".ts") || lower.ends_with(".tsx") {
        Some("typescript")
    } else if lower.ends_with(".js") || lower.ends_with(".jsx") || lower.ends_with(".mjs") {
        Some("javascript")
    } else if lower.ends_with(".py") {
        Some("python")
    } else if lower.ends_with(".go") {
        Some("go")
    } else if lower.ends_with(".java") || lower.ends_with(".kt") {
        Some("java")
    } else if lower.ends_with(".cs") {
        Some("csharp")
    } else if lower.ends_with(".c") {
        Some("c")
    } else if lower.ends_with(".cpp") || lower.ends_with(".cc") || lower.ends_with(".hpp") {
        Some("cpp")
    } else if lower.ends_with(".html") || lower.ends_with(".htm") {
        Some("html")
    } else if lower.ends_with(".css") || lower.ends_with(".scss") {
        Some("css")
    } else if lower.ends_with(".sql") {
        Some("sql")
    } else if lower.ends_with(".sh") || lower.ends_with(".ps1") {
        Some("shell")
    } else {
        None
    };
    if let Some(l) = lang {
        set.insert(l.to_string());
    }
}

/// Recognize well-known framework config files by name (no content read).
fn framework_markers_for_file(name: &str, set: &mut std::collections::BTreeSet<String>) {
    let lower = name.to_ascii_lowercase();
    let marker = if lower.starts_with("vite.config") || lower.starts_with("vite.") {
        Some("vite")
    } else if lower.starts_with("next.config") {
        Some("next.js")
    } else if lower.starts_with("nuxt.config") {
        Some("nuxt")
    } else if lower.starts_with("webpack.config") {
        Some("webpack")
    } else if lower.starts_with("rollup.config") {
        Some("rollup")
    } else if lower.starts_with("svelte.config") {
        Some("svelte")
    } else if lower.starts_with("astro.config") {
        Some("astro")
    } else if lower.starts_with("remix.config") {
        Some("remix")
    } else if lower.starts_with("tailwind.config") {
        Some("tailwind")
    } else if lower.starts_with("angular.json") {
        Some("angular")
    } else if lower == "python_version"
        || lower.starts_with("pytest.ini")
        || lower == "pytest"
        || lower == "conftest.py"
    {
        Some("pytest")
    } else if lower.starts_with("manage.py") {
        Some("django")
    } else if lower.starts_with("app.py") || lower.starts_with("main.py") {
        Some("flask")
    } else if lower.starts_with("docker-compose")
        || lower == "compose.yaml"
        || lower == "compose.yml"
        || lower.starts_with("dockerfile")
    {
        Some("docker")
    } else if lower.starts_with("tauri.conf") {
        Some("tauri")
    } else if lower.starts_with("flutter") || lower == "pubspec.yaml" {
        Some("flutter")
    } else {
        None
    };
    if let Some(m) = marker {
        set.insert(m.to_string());
    }
}

/// Bounded read of a manifest file; returns `None` on any failure.
fn read_bounded(path: &Path, max: usize) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut buf = Vec::with_capacity(max.min(64 * 1024));
    reader
        .by_ref()
        .take(max as u64)
        .read_to_end(&mut buf)
        .ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Extract the package/project name from a manifest, when declared.
fn read_manifest_name(path: &Path, kind: ManifestKind) -> Option<String> {
    let text = read_bounded(path, MAX_MANIFEST_BYTES)?;
    let name = match kind {
        ManifestKind::Cargo => parse_toml_field(&text, "name"),
        ManifestKind::Npm | ManifestKind::Pnpm | ManifestKind::Yarn | ManifestKind::Bun => {
            serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                })
                .or_else(|| parse_json_field(&text, "name"))
        }
        ManifestKind::Pyproject | ManifestKind::Poetry | ManifestKind::Uv => {
            parse_toml_field(&text, "name")
        }
        ManifestKind::GoMod => text.lines().find_map(|l| {
            let l = l.trim();
            l.strip_prefix("module ")
                .map(|m| m.trim().trim_matches('"').to_string())
        }),
        ManifestKind::Maven => parse_xml_field(&text, "artifactId"),
        ManifestKind::Gradle => parse_gradle_field(&text, "rootProject.name"),
        ManifestKind::Csproj => parse_xml_field(&text, "AssemblyName"),
        ManifestKind::Cmake => text.lines().find_map(|l| {
            let l = l.trim();
            l.strip_prefix("project(")
                .map(|p| p.split([')', ',']).next().unwrap_or("").trim().to_string())
                .filter(|s| !s.is_empty())
        }),
        _ => None,
    };
    name.map(|n| crate::utils::truncate(&n, 120))
}

/// `name = "..."` under the first matching section; matches the common TOML
/// manifest shape (`[package]` / `[project]` / `[tool.poetry]`).
fn parse_toml_field(text: &str, field: &str) -> Option<String> {
    let needle = format!("{field} =");
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&needle) {
            let value = rest.trim().trim_matches('"');
            if !value.is_empty() && !value.contains('/') {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// `"name": "..."` from a JSON manifest, without a full JSON parse (bounded).
fn parse_json_field(text: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\"");
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(&needle) {
            if let Some(rest) = trimmed[needle.len()..].strip_prefix(':') {
                let value = rest.trim();
                if let Some(v) = value.strip_prefix('"') {
                    let end = v.find('"')?;
                    let name = &v[..end];
                    if !name.is_empty() && !name.contains('/') {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }
    None
}

/// `<artifactId>...</artifactId>` from an XML manifest.
fn parse_xml_field(text: &str, field: &str) -> Option<String> {
    let start = format!("<{field}>");
    let end = format!("</{field}>");
    text.find(&start).and_then(|pos| {
        let after = &text[pos + start.len()..];
        after.find(&end).map(|e| after[..e].trim().to_string())
    })
}

/// `rootProject.name = "..."` style gradle values.
fn parse_gradle_field(text: &str, field: &str) -> Option<String> {
    let needle = format!("{field} =");
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&needle) {
            let value = rest.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Read dependency names from a root `package.json` (bounded, names only).
fn read_npm_deps(path: &Path) -> Option<Vec<String>> {
    let text = read_bounded(path, MAX_MANIFEST_BYTES)?;
    // Very cheap extraction: find `"dependencies": { ... }` / `"devDependencies"`.
    let mut out: Vec<String> = Vec::new();
    for key in [
        "\"dependencies\"",
        "\"devDependencies\"",
        "\"optionalDependencies\"",
    ] {
        let Some(pos) = text.find(key) else { continue };
        let after = &text[pos + key.len()..];
        let brace = after.find('{')?;
        let body = &after[brace + 1..];
        let close = body.find('}')?;
        for line in body[..close].lines() {
            let line = line.trim().trim_end_matches(',');
            if let Some(rest) = line.strip_prefix('"') {
                if let Some(end) = rest.find('"') {
                    let dep = &rest[..end];
                    if !dep.is_empty() {
                        out.push(dep.to_string());
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    Some(out)
}

/// Map a dependency name to a framework label.
fn dependency_frameworks(dep: &str) -> Vec<&'static str> {
    let lower = dep.to_ascii_lowercase();
    let mut out = Vec::new();
    for (pattern, label) in [
        ("react", "react"),
        ("vue", "vue"),
        ("svelte", "svelte"),
        ("@angular", "angular"),
        ("next", "next.js"),
        ("nuxt", "nuxt"),
        ("vite", "vite"),
        ("webpack", "webpack"),
        ("rollup", "rollup"),
        ("astro", "astro"),
        ("remix", "remix"),
        ("gatsby", "gatsby"),
        ("express", "express"),
        ("fastify", "fastify"),
        ("nestjs", "nest"),
        ("expo", "expo"),
        ("react-native", "react-native"),
        ("tauri", "tauri"),
        ("electron", "electron"),
        ("tailwindcss", "tailwind"),
    ] {
        if lower.starts_with(pattern)
            || lower.contains(&format!("/{pattern}"))
            || lower.contains(&format!("@{pattern}"))
        {
            out.push(label);
        }
    }
    out
}

/// Script *names* from a root `package.json` (never bodies).
fn read_scripts(path: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(text) = read_bounded(path, MAX_MANIFEST_BYTES) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(scripts) = v.get("scripts").and_then(|s| s.as_object()) {
                for (name, _) in scripts {
                    out.push(name.clone());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out.truncate(MAX_SCRIPTS);
    out
}

/// Walk upward from `start` to find the nearest ancestor that contains a
/// `.git` or `.hg` directory.
pub fn detect_repo_root(start: &Path) -> Option<String> {
    let mut current = Some(start);
    while let Some(dir) = current {
        if dir.join(".git").is_dir() || dir.join(".hg").is_dir() {
            return Some(dir.to_string_lossy().into_owned());
        }
        current = dir.parent();
    }
    None
}

/// Read safe git metadata from `.git` files directly (never runs `git`).
fn read_git_metadata(repo_root: &Path) -> RepositoryMetadata {
    let git_dir = repo_root.join(".git");
    if !git_dir.is_dir() {
        return RepositoryMetadata {
            has_git: false,
            branch: None,
            head_ref: None,
            remote_origin: None,
            dirty_state: "not_computed_without_executing_git".to_string(),
        };
    }
    let (branch, head_ref) = read_head(&git_dir);
    let remote_origin = read_git_config(&git_dir)
        .map(|url| redact_url_userinfo(&url))
        .map(|url| redact_value(&url))
        .map(|url| crate::utils::truncate(&url, 200));
    RepositoryMetadata {
        has_git: true,
        branch,
        head_ref,
        remote_origin,
        dirty_state: "not_computed_without_executing_git".to_string(),
    }
}

/// Parse `.git/HEAD`: `ref: refs/heads/main` → branch `main`; a raw commit
/// hash means detached HEAD.
fn read_head(git_dir: &Path) -> (Option<String>, Option<String>) {
    let text = read_bounded(&git_dir.join("HEAD"), MAX_GIT_BYTES);
    let head = text.unwrap_or_default();
    let head = head.trim();
    if let Some(ref_path) = head.strip_prefix("ref: ") {
        let branch = ref_path
            .strip_prefix("refs/heads/")
            .map(|b| b.trim().to_string());
        (branch, Some(ref_path.trim().to_string()))
    } else if !head.is_empty() {
        // Detached HEAD: the file holds a commit hash (bounded to hash length).
        (None, Some(crate::utils::truncate(head, 64)))
    } else {
        (None, None)
    }
}

/// Extract the redacted `origin` remote URL from `.git/config` (bounded read).
fn read_git_config(git_dir: &Path) -> Option<String> {
    let text = read_bounded(&git_dir.join("config"), MAX_GIT_BYTES)?;
    let mut current_remote: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            // `[remote "origin"]` or `[remote "upstream"]`
            let section = line.trim_start_matches('[').trim_end_matches(']');
            if let Some(name) = section.strip_prefix("remote ") {
                current_remote = Some(name.trim().trim_matches('"').to_ascii_lowercase());
            } else {
                current_remote = None;
            }
        } else if current_remote.as_deref() == Some("origin") {
            if let Some(rest) = line.strip_prefix("url =") {
                let url = rest.trim();
                if !url.is_empty() && !url.starts_with('#') {
                    return Some(url.trim_matches('"').to_string());
                }
            }
        }
    }
    None
}

/// Validate and canonicalize a workspace path for scanning: the resolved path
/// must exist, be a directory, and (when `allow_roots` is non-empty) be under
/// one of the allowed roots; it must not be under any deny root. Rejects
/// drive roots (`C:\`) unless the drive root is explicitly allowed, to keep
/// whole-drive scans out unless configured.
pub fn canonicalize_workspace(
    raw: &str,
    allow_roots: &[String],
    deny_roots: &[String],
) -> Result<PathBuf, crate::errors::WinkitError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(crate::errors::WinkitError::path_rejected(
            "workspace path is empty",
        ));
    }
    if raw.len() > 4096 {
        return Err(crate::errors::WinkitError::path_rejected(
            "workspace path is too long",
        ));
    }
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(crate::errors::WinkitError::path_rejected(
            "workspace path must be absolute (e.g. D:\\dev\\MyProject)",
        ));
    }
    let canonical = std::fs::canonicalize(path).map_err(|_| {
        crate::errors::WinkitError::path_rejected(format!(
            "workspace path '{raw}' does not exist or cannot be resolved"
        ))
    })?;
    if !canonical.is_dir() {
        return Err(crate::errors::WinkitError::path_rejected(format!(
            "workspace path '{raw}' is not a directory"
        )));
    }
    // Drive-root (or UNC-root) scans are rejected unless explicitly allowed.
    // `C:\` canonicalizes to the extended-length form `\\?\C:\`, which has no
    // file name component, so a missing file name reliably marks a root path.
    let is_root_path = canonical.file_name().is_none();
    if is_root_path
        && !allow_roots.iter().any(|r| {
            std::fs::canonicalize(r)
                .map(|rc| rc == canonical)
                .unwrap_or(false)
        })
    {
        return Err(crate::errors::WinkitError::path_rejected(format!(
            "workspace path '{raw}' is a filesystem root; whole-drive scans are not allowed \
             (add it to workspaces.allow_roots to permit it explicitly)"
        )));
    }
    if !allow_roots.is_empty() {
        let allowed = allow_roots.iter().any(|r| {
            std::fs::canonicalize(r)
                .map(|rc| canonical.starts_with(&rc))
                .unwrap_or(false)
        });
        if !allowed {
            return Err(crate::errors::WinkitError::path_rejected(format!(
                "workspace path '{raw}' is outside the configured workspaces.allow_roots"
            )));
        }
    }
    for deny in deny_roots {
        if let Ok(denied) = std::fs::canonicalize(deny) {
            if canonical == denied || canonical.starts_with(&denied) {
                return Err(crate::errors::WinkitError::path_rejected(format!(
                    "workspace path '{raw}' is under a configured workspaces.deny_roots entry"
                )));
            }
        }
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorKind;

    /// Unique temp directory for one test.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "winkit-workspace-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn detects_manifests_package_managers_and_languages() {
        let dir = temp_dir("basic");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"demo-app\"\n").unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("package.json"), "{\"name\": \"web-ui\"}\n").unwrap();
        std::fs::write(dir.join("vite.config.ts"), "export default {}\n").unwrap();

        let scan = scan_workspace(&dir, &ScanOptions::default());
        assert!(scan.root_is_valid);
        assert!(scan.package_managers.contains(&"cargo".to_string()));
        assert!(scan.package_managers.contains(&"npm".to_string()));
        assert!(scan.languages.contains(&"rust".to_string()));
        assert!(scan.languages.contains(&"typescript".to_string()));
        assert!(scan.frameworks.contains(&"vite".to_string()));
        let cargo = scan
            .manifests
            .iter()
            .find(|m| m.kind == ManifestKind::Cargo)
            .expect("Cargo.toml detected");
        assert_eq!(cargo.name.as_deref(), Some("demo-app"));
        let npm = scan
            .manifests
            .iter()
            .find(|m| m.kind == ManifestKind::Npm)
            .expect("package.json detected");
        assert_eq!(npm.name.as_deref(), Some("web-ui"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn never_reads_secret_files_but_reports_them() {
        let dir = temp_dir("secrets");
        std::fs::write(dir.join(".env"), "DB_PASSWORD=hunter2\n").unwrap();
        std::fs::write(dir.join("id_rsa"), "PRIVATE KEY MATERIAL\n").unwrap();
        std::fs::write(dir.join("secrets.yaml"), "password: x\n").unwrap();
        std::fs::write(dir.join(".npmrc"), "//registry.npmjs.org/:_authToken=abc\n").unwrap();
        std::fs::write(dir.join("package.json"), "{\"name\": \"ok\"}\n").unwrap();

        let scan = scan_workspace(&dir, &ScanOptions::default());
        assert_eq!(scan.excluded_secret_files.len(), 4);
        assert!(scan.excluded_secret_files.iter().any(|f| f == ".env"));
        assert!(scan.excluded_secret_files.iter().any(|f| f == "id_rsa"));
        // The secret contents never leak into any output field.
        let serialized = serde_json::to_string(&scan).unwrap();
        assert!(!serialized.contains("hunter2"));
        assert!(!serialized.contains("PRIVATE KEY"));
        assert!(!serialized.contains("_authToken"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_metadata_is_read_locally_and_redacted() {
        let dir = temp_dir("git");
        std::fs::create_dir_all(dir.join(".git/refs/heads")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            dir.join(".git/config"),
            "[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = https://alice:supersecret@github.com/acme/demo.git\n",
        )
        .unwrap();

        let scan = scan_workspace(&dir, &ScanOptions::default());
        let repo = &scan.repository;
        assert!(repo.has_git);
        assert_eq!(repo.branch.as_deref(), Some("main"));
        let origin = repo.remote_origin.as_deref().unwrap();
        assert!(origin.starts_with("https://alice:<redacted>@github.com/"));
        assert!(!origin.contains("supersecret"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_repo_root_above_the_workspace() {
        let parent = temp_dir("nested");
        let repo = parent.join("repo");
        let ws = repo.join("packages/web");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/develop\n").unwrap();
        std::fs::write(
            ws.join("package.json"),
            "{\"name\": \"web\", \"scripts\": {\"dev\": \"vite\", \"build\": \"tsc\"}}\n",
        )
        .unwrap();

        let resolved = canonicalize_workspace(&ws.to_string_lossy(), &[], &[]).unwrap();
        let scan = scan_workspace(&resolved, &ScanOptions::default());
        let repo_canonical = repo
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .trim_start_matches("\\\\?\\")
            .to_owned();
        assert_eq!(scan.repo_root.as_deref(), Some(repo_canonical.as_str()));
        assert_eq!(scan.repository.branch.as_deref(), Some("develop"));
        assert_eq!(scan.scripts, vec!["build".to_string(), "dev".to_string()]);
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn bounds_the_walk_and_reports_truncation() {
        let dir = temp_dir("bounded");
        for i in 0..10 {
            let sub = dir.join(format!("d{i}"));
            std::fs::create_dir_all(&sub).unwrap();
            for j in 0..10 {
                std::fs::write(sub.join(format!("f{j}.txt")), "x").unwrap();
            }
        }
        let options = ScanOptions {
            max_depth: 2,
            max_files: 25,
            ..ScanOptions::default()
        };
        let scan = scan_workspace(&dir, &options);
        assert!(scan.entries_scanned <= 25);
        assert!(scan.truncated);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strips_extended_length_prefix_from_reported_root() {
        let dir = temp_dir("prefix");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let input = if cfg!(windows) {
            // Canonicalization on Windows produces this extended-length form.
            PathBuf::from(format!("\\\\?\\{}", dir.display()))
        } else {
            dir.clone()
        };
        let scan = scan_workspace(&input, &ScanOptions::default());
        assert_eq!(scan.root, dir.display().to_string());
        assert_eq!(
            scan.display_name,
            dir.file_name().unwrap().to_string_lossy().into_owned()
        );
        let cargo = scan
            .manifests
            .iter()
            .find(|m| m.kind == ManifestKind::Cargo)
            .expect("manifest still detected with prefixed root");
        assert_eq!(cargo.name.as_deref(), Some("x"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_product_and_vendor_directories() {
        let dir = temp_dir("skip");
        std::fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::write(dir.join("node_modules/pkg/index.js"), "x").unwrap();
        std::fs::write(dir.join("target/debug/app.exe"), "x").unwrap();
        let scan = scan_workspace(&dir, &ScanOptions::default());
        assert!(scan.build_dirs.iter().any(|d| d == "node_modules"));
        assert!(scan.build_dirs.iter().any(|d| d == "target"));
        assert_eq!(scan.manifests.len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn canonicalize_rejects_missing_relative_and_root_paths() {
        let missing = canonicalize_workspace("C:\\definitely\\not\\here", &[], &[]);
        assert_eq!(missing.unwrap_err().kind, ErrorKind::PathRejected);

        let relative = canonicalize_workspace("relative/path", &[], &[]);
        assert_eq!(relative.unwrap_err().kind, ErrorKind::PathRejected);

        let drive_root: PathBuf = "C:\\".into();
        let rejected = canonicalize_workspace(&drive_root.to_string_lossy(), &[], &[]);
        assert_eq!(rejected.unwrap_err().kind, ErrorKind::PathRejected);
    }

    #[test]
    fn canonicalize_enforces_allow_and_deny_roots() {
        let dir = temp_dir("policy");
        let ws = dir.join("allowed");
        std::fs::create_dir_all(&ws).unwrap();
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();

        let res = canonicalize_workspace(
            &other.to_string_lossy(),
            &[ws.to_string_lossy().into_owned()],
            &[],
        );
        assert_eq!(res.unwrap_err().kind, ErrorKind::PathRejected);

        let res = canonicalize_workspace(
            &ws.to_string_lossy(),
            &[ws.to_string_lossy().into_owned()],
            &[],
        );
        assert!(res.is_ok());

        let res = canonicalize_workspace(
            &ws.to_string_lossy(),
            &[],
            &[ws.to_string_lossy().into_owned()],
        );
        assert_eq!(res.unwrap_err().kind, ErrorKind::PathRejected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dependency_framework_mapping_covers_common_stacks() {
        assert!(dependency_frameworks("react").contains(&"react"));
        assert!(dependency_frameworks("@vitejs/plugin-react").contains(&"vite"));
        assert!(dependency_frameworks("next").contains(&"next.js"));
        assert!(dependency_frameworks("express").contains(&"express"));
        assert!(dependency_frameworks("lodash").is_empty());
    }

    #[test]
    fn manifest_reads_are_dependency_names_not_bodies() {
        let dir = temp_dir("deps");
        std::fs::write(
            dir.join("package.json"),
            "{\"name\":\"app\",\"scripts\":{\"dev\":\"vite --port 3000\",\"deploy\":\"echo SECRET_TOKEN_VALUE\"},\"dependencies\":{\"react\":\"^18\"}}\n",
        )
        .unwrap();
        let scan = scan_workspace(&dir, &ScanOptions::default());
        assert!(scan.scripts.contains(&"dev".to_string()));
        // Script bodies never appear.
        let serialized = serde_json::to_string(&scan).unwrap();
        assert!(!serialized.contains("vite --port 3000"));
        assert!(!serialized.contains("SECRET_TOKEN_VALUE"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
