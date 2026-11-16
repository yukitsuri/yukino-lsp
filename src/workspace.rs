use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use serde::Deserialize;
use yukic_ast::{ImportPath, ItemKind, Module, Visibility};
use yukic_lexer::Lexer;
use yukic_parser::Parser;
use yukic_resolve::{find_module_file_for_path, SearchPaths};
use yukic_span::FileId;

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct LspSection {
    #[serde(default)]
    pub allow_undef: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct AppManifest {
    pub bin: BinSection,
    #[serde(default)]
    pub libs: LibsSection,
    #[serde(default)]
    pub lsp: LspSection,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct LibManifest {
    #[serde(alias = "package")]
    pub lib: LibSection,
    #[serde(default)]
    pub libs: LibsSection,
    #[serde(default)]
    pub lsp: LspSection,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct BinSection {
    pub name: String,
    #[serde(default)]
    pub entry: Option<PathBuf>,
    #[serde(default = "default_includes")]
    pub includes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct LibSection {
    pub name: String,
    #[serde(default)]
    pub entry: Option<PathBuf>,
    #[serde(default = "default_includes")]
    pub includes: Vec<String>,
}

fn default_includes() -> Vec<String> {
    vec!["stdlib".to_string(), "core".to_string()]
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct LibsSection {
    #[serde(default)]
    pub runtime: HashMap<String, DependencySpec>,
    #[serde(default)]
    pub build: HashMap<String, DependencySpec>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct DependencySpec {
    pub path: Option<PathBuf>,
    pub git: Option<String>,
    pub tag: Option<String>,
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub link: Option<String>,
    pub checksum: Option<String>,
    #[serde(default, alias = "sublib", alias = "subpath", alias = "lib")]
    pub package: Option<String>,
    #[serde(default)]
    pub reexport: Option<bool>,
    #[serde(default)]
    pub splitnames: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct WorkspaceManifest {
    pub workspace: WorkspaceSection,
    #[serde(default)]
    pub members: HashMap<String, WorkspaceMemberPackage>,
    #[serde(default)]
    pub lsp: LspSection,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct WorkspaceSection {
    pub name: Option<String>,
    #[serde(default)]
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct WorkspaceMemberPackage {
    #[serde(default, alias = "type", alias = "output")]
    pub kind: String,
    #[serde(default)]
    pub entry: Option<PathBuf>,
    #[serde(default = "default_includes")]
    pub includes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ExportedSymbol {
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub doc: Option<String>,
    pub span: yukic_span::Span,
    pub file_path: PathBuf,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct CachedModule {
    pub path: PathBuf,
    pub mtime: SystemTime,
    pub module: Module,
    pub exports: Vec<ExportedSymbol>,
}

#[derive(Clone)]
pub struct WorkspaceEnv {
    pub project_root: PathBuf,
    pub search_paths: SearchPaths,
    pub available_libs: Vec<String>,
    pub splitnames: HashMap<String, Vec<String>>,
    pub allow_undef: HashSet<String>,
    cache: Arc<RwLock<HashMap<PathBuf, CachedModule>>>,
}

impl WorkspaceEnv {
    pub fn for_path(current_file: &Path) -> Self {
        let (project_root, manifest_type) = find_project_root_and_manifest(current_file);
        let work_dir = project_root.join("yuki-work/local");

        // If cache directory is absent, compile project using yukino compiler
        if !work_dir.exists() {
            trigger_project_build(&project_root);
        }

        let cache_base = resolve_cache_base(&project_root);
        let stdlib_dir = cache_base.join("stdlib");
        let core_dir = cache_base.join("core");
        let macrosdk_dir = cache_base.join("macrosdk");

        ensure_unpacked(&stdlib_dir, yukino_builtin::unpack_stdlib_source);
        ensure_unpacked(&core_dir, yukino_builtin::unpack_core_source);
        ensure_unpacked(&macrosdk_dir, yukino_builtin::unpack_macrosdk_source);

        let mut search_paths = SearchPaths::new();
        let mut available_libs = Vec::new();
        let mut splitnames: HashMap<String, Vec<String>> = HashMap::new();

        let includes = manifest_type.includes();
        if includes.iter().any(|i| i == "stdlib") {
            search_paths.add_lib("std", &stdlib_dir);
            available_libs.push("std".to_string());
        }
        if includes.iter().any(|i| i == "core") {
            search_paths.add_lib("core", &core_dir);
            available_libs.push("core".to_string());
        }
        if includes.iter().any(|i| i == "macrosdk") {
            search_paths.add_lib("macrosdk", &macrosdk_dir);
            available_libs.push("macrosdk".to_string());
        }

        load_dependencies(
            &project_root,
            &manifest_type,
            &mut search_paths,
            &mut available_libs,
            &mut splitnames,
        );

        available_libs.sort();
        available_libs.dedup();

        let mut allow_undef = HashSet::new();
        for sym in manifest_type.allow_undef() {
            allow_undef.insert(sym);
        }

        let mut curr = project_root.parent();
        while let Some(parent) = curr {
            let ws_file = parent.join("yukispace.toml");
            if ws_file.is_file() {
                if let Ok(text) = fs::read_to_string(&ws_file) {
                    if let Ok(m) = toml::from_str::<WorkspaceManifest>(&text) {
                        for sym in m.lsp.allow_undef {
                            allow_undef.insert(sym);
                        }
                    }
                }
            }
            if parent.join(".git").is_dir() {
                break;
            }
            curr = parent.parent();
        }

        Self {
            project_root,
            search_paths,
            available_libs,
            splitnames,
            allow_undef,
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn resolve_lib_entry_file(&self, pkg_name: &str) -> Option<PathBuf> {
        let dir = self.search_paths.find_lib(pkg_name)?;

        // 1. Check lib.toml for entry
        if let Ok(text) = fs::read_to_string(dir.join("lib.toml")) {
            if let Ok(manifest) = toml::from_str::<LibManifest>(&text) {
                if let Some(ref entry) = manifest.lib.entry {
                    let cand = dir.join(entry);
                    if cand.is_file() {
                        return Some(cand);
                    }
                }
            }
        }
        // 2. Check yuki.toml for entry
        if let Ok(text) = fs::read_to_string(dir.join("yuki.toml")) {
            if let Ok(manifest) = toml::from_str::<AppManifest>(&text) {
                if let Some(ref entry) = manifest.bin.entry {
                    let cand = dir.join(entry);
                    if cand.is_file() {
                        return Some(cand);
                    }
                }
            }
        }

        // 3. Check conventional filenames
        let candidates = [
            dir.join("lib.ykn"),
            dir.join(pkg_name).join("lib.ykn"),
            dir.join("core").join("lib.ykn"),
            dir.join("mod.ykn"),
            dir.join("src").join("lib.ykn"),
            dir.join("src").join("main.ykn"),
        ];
        for cand in candidates {
            if cand.is_file() {
                return Some(cand);
            }
        }

        None
    }

    pub fn resolve_import(&self, import_path: &ImportPath, current_file: &Path) -> Option<PathBuf> {
        if let Some(p) = find_module_file_for_path(
            import_path,
            &self.project_root,
            current_file,
            &self.search_paths,
        ) {
            return Some(p);
        }

        // Check splitnames if it's a package import
        if let yukic_ast::PathPrefix::Package(ref id) = import_path.prefix {
            if let Some(splits) = self.splitnames.get(&id.name) {
                for split_pkg in splits {
                    let mut split_import = import_path.clone();
                    split_import.prefix = yukic_ast::PathPrefix::Package(yukic_ast::Ident::new(
                        split_pkg,
                        id.span,
                    ));
                    if let Some(p) = find_module_file_for_path(
                        &split_import,
                        &self.project_root,
                        current_file,
                        &self.search_paths,
                    ) {
                        return Some(p);
                    }
                    if split_import.segments.is_empty() {
                        if let Some(p) = self.resolve_lib_entry_file(split_pkg) {
                            return Some(p);
                        }
                    }
                }
            }
            if import_path.segments.is_empty() {
                if let Some(p) = self.resolve_lib_entry_file(&id.name) {
                    return Some(p);
                }
            }
        }

        None
    }

    pub fn get_or_parse_module(&self, path: &Path) -> Option<CachedModule> {
        self.get_or_parse_module_depth(path, 0)
    }

    fn get_or_parse_module_depth(&self, path: &Path, depth: usize) -> Option<CachedModule> {
        let metadata = fs::metadata(path).ok()?;
        let mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        {
            let lock = self.cache.read().ok()?;
            if let Some(cached) = lock.get(path) {
                if cached.mtime == mtime {
                    return Some(cached.clone());
                }
            }
        }

        let content = fs::read_to_string(path).ok()?;
        let lexer = Lexer::new(FileId(0), &content);
        let (tokens, _) = lexer.tokenize();
        let mut parser = Parser::with_file_id(tokens, FileId(0));
        let (module, _) = parser.parse_module();

        let mut exports = extract_exports(&module, &content, path);

        // Follow public re-exports (e.g. `public load .io: *` in std/lib.ykn)
        if depth < 5 {
            for item in &module.items {
                if item.visibility == Visibility::Public {
                    if let ItemKind::Import(ref imp) = item.kind {
                        if let Some(target_file) = self.resolve_import(&imp.path, path) {
                            if target_file != path {
                                if let Some(target_mod) =
                                    self.get_or_parse_module_depth(&target_file, depth + 1)
                                {
                                    for exp in target_mod.exports {
                                        let matched = match &imp.symbols {
                                            None => true,
                                            Some(clause) => clause
                                                .items
                                                .iter()
                                                .any(|it| it.pattern.matches(&exp.name)),
                                        };
                                        if matched && !exports.iter().any(|e| e.name == exp.name) {
                                            exports.push(exp);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Merge splitnames exports if this file is the entry point of a package with splitnames
            for (base_pkg, splits) in &self.splitnames {
                let base_entry = self.resolve_lib_entry_file(base_pkg);
                if base_entry.as_deref() == Some(path) {
                    for split_pkg in splits {
                        if let Some(split_entry) = self.resolve_lib_entry_file(split_pkg) {
                            if split_entry != *path {
                                if let Some(split_mod) =
                                    self.get_or_parse_module_depth(&split_entry, depth + 1)
                                {
                                    for exp in split_mod.exports {
                                        if !exports.iter().any(|e| e.name == exp.name) {
                                            exports.push(exp);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let cached = CachedModule {
            path: path.to_path_buf(),
            mtime,
            module,
            exports,
        };

        if let Ok(mut lock) = self.cache.write() {
            lock.insert(path.to_path_buf(), cached.clone());
        }

        Some(cached)
    }

    pub fn list_package_submodules(&self, pkg_name: &str) -> Vec<String> {
        let mut submodules = Vec::new();
        let parts: Vec<&str> = pkg_name.split('.').collect();
        let first = parts[0];
        let base_dir = match self.search_paths.find_lib(first) {
            Some(dir) => {
                let candidate = dir.join(first);
                if candidate.is_dir() {
                    candidate
                } else {
                    dir.to_path_buf()
                }
            }
            None => return submodules,
        };

        let mut target_dir = base_dir;
        for part in &parts[1..] {
            target_dir = target_dir.join(part);
        }

        if let Ok(entries) = fs::read_dir(target_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if stem.is_empty() || stem == "lib" || stem == "mod" {
                    continue;
                }
                if p.is_file() && p.extension().is_some_and(|e| e == "ykn") {
                    submodules.push(stem.to_string());
                } else if p.is_dir() {
                    submodules.push(stem.to_string());
                }
            }
        }

        submodules.sort();
        submodules.dedup();
        submodules
    }
}

pub fn scan_spes_classes(dir: &Path) -> Vec<String> {
    let mut classes = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() && p.extension().is_some_and(|e| e == "spes") {
                if let Ok(content) = fs::read_to_string(&p) {
                    for line in content.lines() {
                        let trimmed = line.trim();
                        let name_opt = if let Some(rest) = trimmed.strip_prefix("theme ") {
                            extract_spes_ident(rest)
                        } else if let Some(rest) = trimmed.strip_prefix("screen ") {
                            extract_spes_ident(rest)
                        } else if let Some(rest) = trimmed.strip_prefix("dialog ") {
                            extract_spes_ident(rest)
                        } else if let Some(rest) = trimmed.strip_prefix("component ") {
                            extract_spes_ident(rest)
                        } else if let Some(rest) = trimmed.strip_prefix("use ") {
                            extract_spes_ident(rest)
                        } else {
                            None
                        };
                        if let Some(name) = name_opt {
                            let pascal = to_pascal_case(&name);
                            if !pascal.is_empty() {
                                classes.push(pascal);
                            }
                        }
                    }
                }
            }
        }
    }
    classes.sort();
    classes.dedup();
    classes
}

fn extract_spes_ident(s: &str) -> Option<String> {
    let id: String = s
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

fn to_pascal_case(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap().to_uppercase().to_string();
    format!("{}{}", first, chars.as_str())
}

pub enum LoadedManifest {
    App(AppManifest),
    Lib(LibManifest),
    Workspace(WorkspaceManifest),
    None,
}

impl LoadedManifest {
    pub fn includes(&self) -> Vec<String> {
        match self {
            Self::App(a) => a.bin.includes.clone(),
            Self::Lib(l) => l.lib.includes.clone(),
            Self::Workspace(_) | Self::None => default_includes(),
        }
    }

    pub fn allow_undef(&self) -> Vec<String> {
        match self {
            Self::App(a) => a.lsp.allow_undef.clone(),
            Self::Lib(l) => l.lsp.allow_undef.clone(),
            Self::Workspace(w) => w.lsp.allow_undef.clone(),
            Self::None => Vec::new(),
        }
    }

    pub fn is_present(&self) -> bool {
        !matches!(self, Self::None)
    }
}

pub fn find_project_root_and_manifest(file_path: &Path) -> (PathBuf, LoadedManifest) {
    let mut current = if file_path.is_file() {
        file_path.parent().unwrap_or(Path::new("."))
    } else {
        file_path
    };

    let fallback_root = current.to_path_buf();

    loop {
        let app_file = current.join("yuki.toml");
        if app_file.is_file() {
            if let Ok(text) = fs::read_to_string(&app_file) {
                if let Ok(m) = toml::from_str::<AppManifest>(&text) {
                    return (current.to_path_buf(), LoadedManifest::App(m));
                }
                if let Ok(m) = toml::from_str::<LibManifest>(&text) {
                    return (current.to_path_buf(), LoadedManifest::Lib(m));
                }
            }
            return (current.to_path_buf(), LoadedManifest::None);
        }

        let lib_file = current.join("lib.toml");
        if lib_file.is_file() {
            if let Ok(text) = fs::read_to_string(&lib_file) {
                if let Ok(m) = toml::from_str::<LibManifest>(&text) {
                    return (current.to_path_buf(), LoadedManifest::Lib(m));
                }
            }
            return (current.to_path_buf(), LoadedManifest::None);
        }

        let ws_file = current.join("yukispace.toml");
        if ws_file.is_file() {
            if let Ok(text) = fs::read_to_string(&ws_file) {
                if let Ok(m) = toml::from_str::<WorkspaceManifest>(&text) {
                    return (current.to_path_buf(), LoadedManifest::Workspace(m));
                }
            }
            return (current.to_path_buf(), LoadedManifest::None);
        }

        if current.join(".git").is_dir() {
            return (current.to_path_buf(), LoadedManifest::None);
        }

        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }

    (fallback_root, LoadedManifest::None)
}

fn trigger_project_build(project_root: &Path) {
    let compiler = if which("yukino") {
        "yukino"
    } else if which("yukino-dbg") {
        "yukino-dbg"
    } else {
        return;
    };

    let _ = Command::new(compiler)
        .arg("build")
        .arg("--no-out")
        .current_dir(project_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn which(bin_name: &str) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if dir.join(bin_name).is_file() {
                return true;
            }
        }
    }
    false
}

fn resolve_cache_base(project_root: &Path) -> PathBuf {
    let local = project_root.join("yuki-work/local");
    if local.is_dir() || project_root.join("yuki.toml").is_file() || project_root.join("lib.toml").is_file() {
        let _ = fs::create_dir_all(&local);
        return local;
    }

    let global_cache = std::env::temp_dir().join("yukino_cache");
    let _ = fs::create_dir_all(&global_cache);
    global_cache
}

fn ensure_unpacked<F>(dir: &Path, unpack_fn: F)
where
    F: FnOnce(&Path) -> Result<PathBuf, yukino_builtin::BuiltinError>,
{
    if !dir.exists() || !dir.join(".source_hash").is_file() {
        let _ = unpack_fn(dir);
    }
}

fn load_dependencies(
    project_root: &Path,
    manifest: &LoadedManifest,
    search_paths: &mut SearchPaths,
    available_libs: &mut Vec<String>,
    splitnames: &mut HashMap<String, Vec<String>>,
) {
    let local_deps_dir = project_root.join("yuki-work/local/deps");
    if local_deps_dir.is_dir() {
        search_paths.add_search_dir(&local_deps_dir);
    }

    let deps_dir = project_root.join("yuki-work/deps");
    if deps_dir.is_dir() {
        search_paths.add_search_dir(&deps_dir);
    }

    let offlibs_dir = project_root.join("source/offlibs");
    if offlibs_dir.is_dir() {
        search_paths.add_search_dir(&offlibs_dir);
    }

    // Check ancestor directories for enclosing yukispace.toml
    let mut dir = project_root.to_path_buf();
    while let Some(parent) = dir.parent() {
        let ws_file = parent.join("yukispace.toml");
        if ws_file.is_file() {
            if let Ok(text) = fs::read_to_string(&ws_file) {
                if let Ok(ws) = toml::from_str::<WorkspaceManifest>(&text) {
                    register_workspace_members(&ws, parent, search_paths, available_libs);
                }
            }
            break;
        }
        dir = parent.to_path_buf();
    }

    // Queue-based transitive dependency resolution
    let mut queue: VecDeque<(String, DependencySpec, PathBuf)> = VecDeque::new();

    match manifest {
        LoadedManifest::App(a) => {
            for (alias, spec) in &a.libs.runtime {
                queue.push_back((alias.clone(), spec.clone(), project_root.to_path_buf()));
            }
        }
        LoadedManifest::Lib(l) => {
            available_libs.push(l.lib.name.clone());
            search_paths.add_lib(&l.lib.name, project_root);
            for (alias, spec) in &l.libs.runtime {
                queue.push_back((alias.clone(), spec.clone(), project_root.to_path_buf()));
            }
        }
        LoadedManifest::Workspace(ws) => {
            register_workspace_members(ws, project_root, search_paths, available_libs);
        }
        LoadedManifest::None => {}
    }

    let ws_file = project_root.join("yukispace.toml");
    if ws_file.is_file() && !matches!(manifest, LoadedManifest::Workspace(_)) {
        if let Ok(text) = fs::read_to_string(&ws_file) {
            if let Ok(ws) = toml::from_str::<WorkspaceManifest>(&text) {
                register_workspace_members(&ws, project_root, search_paths, available_libs);
            }
        }
    }

    let mut visited: HashSet<(String, PathBuf)> = HashSet::new();

    while let Some((alias, spec, base_dir)) = queue.pop_front() {
        let raw_target_dir = if let Some(ref path) = spec.path {
            let direct = base_dir.join(path);
            direct.canonicalize().unwrap_or(direct)
        } else {
            let local_dep_path = project_root.join("yuki-work/local/deps").join(&alias);
            let legacy_dep_path = project_root.join("yuki-work/deps").join(&alias);
            if local_dep_path.is_dir() {
                local_dep_path
            } else if legacy_dep_path.is_dir() {
                legacy_dep_path
            } else {
                continue;
            }
        };

        let target_dir = if raw_target_dir.join("yukispace.toml").is_file() {
            if let Ok(text) = fs::read_to_string(raw_target_dir.join("yukispace.toml")) {
                if let Ok(ws) = toml::from_str::<WorkspaceManifest>(&text) {
                    register_workspace_members(&ws, &raw_target_dir, search_paths, available_libs);
                }
            }
            let pkg_to_find = spec.package.as_deref().unwrap_or(&alias);
            find_workspace_member_dir(&raw_target_dir, pkg_to_find).unwrap_or(raw_target_dir)
        } else {
            raw_target_dir
        };

        if !visited.insert((alias.clone(), target_dir.clone())) {
            continue;
        }

        available_libs.push(alias.clone());
        search_paths.add_lib(&alias, &target_dir);

        let sub_manifest_path = if target_dir.join("lib.toml").is_file() {
            Some(target_dir.join("lib.toml"))
        } else if target_dir.join("yuki.toml").is_file() {
            Some(target_dir.join("yuki.toml"))
        } else {
            None
        };

        if let Some(sub_path) = sub_manifest_path {
            if let Ok(sub_text) = fs::read_to_string(&sub_path) {
                if let Ok(sub_lib) = toml::from_str::<LibManifest>(&sub_text) {
                    for (child_name, child_spec) in sub_lib.libs.runtime {
                        let is_split = child_spec.splitnames.unwrap_or(false)
                            || child_spec.reexport.unwrap_or(false);
                        if is_split {
                            splitnames
                                .entry(alias.clone())
                                .or_default()
                                .push(child_name.clone());
                        }
                        queue.push_back((child_name, child_spec, target_dir.clone()));
                    }
                } else if let Ok(sub_app) = toml::from_str::<AppManifest>(&sub_text) {
                    for (child_name, child_spec) in sub_app.libs.runtime {
                        let is_split = child_spec.splitnames.unwrap_or(false)
                            || child_spec.reexport.unwrap_or(false);
                        if is_split {
                            splitnames
                                .entry(alias.clone())
                                .or_default()
                                .push(child_name.clone());
                        }
                        queue.push_back((child_name, child_spec, target_dir.clone()));
                    }
                }
            }
        }
    }

    // Transitive closure of splitnames
    let mut changed = true;
    while changed {
        changed = false;
        let keys: Vec<String> = splitnames.keys().cloned().collect();
        for base in keys {
            let current = splitnames.get(&base).cloned().unwrap_or_default();
            for split in current {
                if let Some(nested) = splitnames.get(&split).cloned() {
                    let target_list = splitnames.entry(base.clone()).or_default();
                    for n in nested {
                        if !target_list.contains(&n) {
                            target_list.push(n);
                            changed = true;
                        }
                    }
                }
            }
        }
    }
}

fn register_workspace_members(
    ws: &WorkspaceManifest,
    project_root: &Path,
    search_paths: &mut SearchPaths,
    available_libs: &mut Vec<String>,
) {
    for pattern in &ws.workspace.members {
        if pattern.ends_with("/*") {
            let parent = pattern.trim_end_matches("/*");
            let parent_dir = project_root.join(parent);
            if let Ok(entries) = fs::read_dir(&parent_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let pkg_name = read_package_name_from_dir(&path)
                            .or_else(|| path.file_name().and_then(|s| s.to_str()).map(|s| s.to_string()));
                        if let Some(name) = pkg_name {
                            if name != "core" && name != "std" && name != "macrosdk" {
                                available_libs.push(name.clone());
                                search_paths.add_lib(&name, &path);
                            }
                        }
                    }
                }
            }
        } else {
            let member_dir = project_root.join(pattern);
            if member_dir.is_dir() {
                let pkg_name = read_package_name_from_dir(&member_dir)
                    .or_else(|| member_dir.file_name().and_then(|s| s.to_str()).map(|s| s.to_string()));
                if let Some(name) = pkg_name {
                    if name != "core" && name != "std" && name != "macrosdk" {
                        available_libs.push(name.clone());
                        search_paths.add_lib(&name, &member_dir);
                    }
                }
            }
        }
    }

    for (name, pkg) in &ws.members {
        available_libs.push(name.clone());
        let entry_dir = if let Some(ref entry) = pkg.entry {
            project_root.join(entry)
        } else {
            project_root.join(name)
        };
        if let Some(parent) = entry_dir.parent() {
            search_paths.add_lib(name, parent);
        } else {
            search_paths.add_lib(name, &entry_dir);
        }
    }
}

fn find_workspace_member_dir(ws_dir: &Path, pkg_name: &str) -> Option<PathBuf> {
    let ws_file = ws_dir.join("yukispace.toml");
    let text = fs::read_to_string(&ws_file).ok()?;
    let ws = toml::from_str::<WorkspaceManifest>(&text).ok()?;
    for pattern in &ws.workspace.members {
        if pattern.ends_with("/*") {
            let parent = pattern.trim_end_matches("/*");
            let parent_dir = ws_dir.join(parent);
            if let Ok(entries) = fs::read_dir(&parent_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let name = read_package_name_from_dir(&path)
                            .or_else(|| path.file_name().and_then(|s| s.to_str()).map(|s| s.to_string()));
                        if let Some(ref n) = name {
                            if n == pkg_name {
                                return Some(path);
                            }
                        }
                    }
                }
            }
        } else {
            let member_dir = ws_dir.join(pattern);
            if member_dir.is_dir() {
                let name = read_package_name_from_dir(&member_dir)
                    .or_else(|| member_dir.file_name().and_then(|s| s.to_str()).map(|s| s.to_string()));
                if let Some(ref n) = name {
                    if n == pkg_name {
                        return Some(member_dir);
                    }
                }
            }
        }
    }
    None
}

fn read_package_name_from_dir(path: &Path) -> Option<String> {
    if let Ok(text) = fs::read_to_string(path.join("lib.toml")) {
        if let Ok(m) = toml::from_str::<LibManifest>(&text) {
            return Some(m.lib.name);
        }
    }
    if let Ok(text) = fs::read_to_string(path.join("yuki.toml")) {
        if let Ok(m) = toml::from_str::<AppManifest>(&text) {
            return Some(m.bin.name);
        }
        if let Ok(m) = toml::from_str::<LibManifest>(&text) {
            return Some(m.lib.name);
        }
    }
    None
}

fn extract_exports(module: &Module, _source: &str, file_path: &Path) -> Vec<ExportedSymbol> {
    let mut exports = Vec::new();

    for item in &module.items {
        if item.visibility != Visibility::Public {
            continue;
        }

        match &item.kind {
            ItemKind::Fun(ref f) => {
                let params: Vec<String> = f
                    .params
                    .iter()
                    .map(|p| {
                        let mut s = String::new();
                        if p.is_ref && p.is_mut {
                            s.push_str("ref ~");
                        } else if p.is_ref {
                            s.push_str("ref ");
                        }
                        s.push_str(&p.name.name);
                        if let Some(ref ty) = p.ty {
                            s.push_str(": ");
                            s.push_str(&ty.to_string());
                        }
                        if p.is_variadic {
                            s.push_str("...");
                        }
                        s
                    })
                    .collect();
                let ret = if let Some(ref rt) = f.return_type {
                    format!(" -> {rt}")
                } else {
                    String::new()
                };
                exports.push(ExportedSymbol {
                    name: f.name.name.clone(),
                    kind: "function".to_string(),
                    signature: format!("fun {}({}){}", f.name.name, params.join(", "), ret),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Cfun(ref c) => {
                let params: Vec<String> = c
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", p.name.name, p.ty))
                    .collect();
                let ret = if let Some(ref rt) = c.return_type {
                    format!(" -> {rt}")
                } else {
                    " -> void".to_string()
                };
                exports.push(ExportedSymbol {
                    name: c.name.name.clone(),
                    kind: "cfun".to_string(),
                    signature: format!("cfun {}({}){};", c.name.name, params.join(", "), ret),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Struct(ref s) => {
                let fields: Vec<String> = s
                    .fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name.name, f.ty))
                    .collect();
                let signature = if fields.is_empty() {
                    format!("struct {}", s.name.name)
                } else {
                    format!("struct {} {{ {} }}", s.name.name, fields.join(", "))
                };
                exports.push(ExportedSymbol {
                    name: s.name.name.clone(),
                    kind: "struct".to_string(),
                    signature,
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Cstruct(ref cs) => {
                let fields: Vec<String> = cs
                    .fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name.name, f.ty))
                    .collect();
                let signature = if fields.is_empty() {
                    format!("cstruct {}", cs.name.name)
                } else {
                    format!("cstruct {} {{ {} }}", cs.name.name, fields.join(", "))
                };
                exports.push(ExportedSymbol {
                    name: cs.name.name.clone(),
                    kind: "cstruct".to_string(),
                    signature,
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Class(ref c) => {
                let prefix = if c.is_locked { "locked class" } else { "class" };
                exports.push(ExportedSymbol {
                    name: c.name.name.clone(),
                    kind: "class".to_string(),
                    signature: format!("{prefix} {}", c.name.name),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Enum(ref e) => {
                let variants: Vec<String> = e
                    .variants
                    .iter()
                    .map(|v| match v {
                        yukic_ast::EnumVariant::Simple(id) => id.name.clone(),
                        yukic_ast::EnumVariant::WithValue(id, _) => format!("{} = ...", id.name),
                        yukic_ast::EnumVariant::Adt(id, fields) => {
                            let fld_strs: Vec<String> = fields
                                .iter()
                                .map(|f| format!("{}: {}", f.name.name, f.ty))
                                .collect();
                            format!("{}({})", id.name, fld_strs.join(", "))
                        }
                    })
                    .collect();
                let signature = if variants.is_empty() {
                    format!("enum {}", e.name.name)
                } else {
                    format!("enum {} {{ {} }}", e.name.name, variants.join(", "))
                };
                exports.push(ExportedSymbol {
                    name: e.name.name.clone(),
                    kind: "enum".to_string(),
                    signature,
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Interface(ref i) => {
                exports.push(ExportedSymbol {
                    name: i.name.name.clone(),
                    kind: "interface".to_string(),
                    signature: format!("interface {}", i.name.name),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Static(ref s) => {
                let ty_str = if let Some(ref ty) = s.ty {
                    format!(": {ty}")
                } else {
                    String::new()
                };
                exports.push(ExportedSymbol {
                    name: s.name.name.clone(),
                    kind: "constant".to_string(),
                    signature: format!("static {}{}", s.name.name, ty_str),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Macro(ref m) => {
                exports.push(ExportedSymbol {
                    name: m.name.name.clone(),
                    kind: "macro".to_string(),
                    signature: format!("define {}!(...)", m.name.name),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::TypeAlias(ref ta) => {
                exports.push(ExportedSymbol {
                    name: ta.name.name.clone(),
                    kind: "type".to_string(),
                    signature: format!("type {} = {}", ta.name.name, ta.target),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Alias(ref a) => {
                exports.push(ExportedSymbol {
                    name: a.alias.name.clone(),
                    kind: "alias".to_string(),
                    signature: format!("alias as {}", a.alias.name),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Externc(ref ext) => {
                for ext_item in &ext.items {
                    match ext_item {
                        yukic_ast::ExterncItem::Cfun(ref c) => {
                            let params: Vec<String> = c
                                .params
                                .iter()
                                .map(|p| format!("{}: {}", p.name.name, p.ty))
                                .collect();
                            let ret = if let Some(ref rt) = c.return_type {
                                format!(" -> {rt}")
                            } else {
                                " -> void".to_string()
                            };
                            exports.push(ExportedSymbol {
                                name: c.name.name.clone(),
                                kind: "cfun".to_string(),
                                signature: format!(
                                    "cfun {}({}){};",
                                    c.name.name,
                                    params.join(", "),
                                    ret
                                ),
                                doc: None,
                                span: c.span,
                                file_path: file_path.to_path_buf(),
                            });
                        }
                        yukic_ast::ExterncItem::Cstruct(ref cs) => {
                            let fields: Vec<String> = cs
                                .fields
                                .iter()
                                .map(|f| format!("{}: {}", f.name.name, f.ty))
                                .collect();
                            let signature = if fields.is_empty() {
                                format!("cstruct {}", cs.name.name)
                            } else {
                                format!("cstruct {} {{ {} }}", cs.name.name, fields.join(", "))
                            };
                            exports.push(ExportedSymbol {
                                name: cs.name.name.clone(),
                                kind: "cstruct".to_string(),
                                signature,
                                doc: None,
                                span: cs.span,
                                file_path: file_path.to_path_buf(),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    exports
}
